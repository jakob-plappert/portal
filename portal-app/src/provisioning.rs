use std::cmp::Ordering;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::runpod_infrastructure::{
    BillingSummary, CreateEndpointRequest, CreateNetworkVolumeRequest, DataCenter, Endpoint,
    EndpointGpu, EndpointScaling, EndpointWorkers, GpuType, NetworkVolume,
    RunPodInfrastructureClient, UpdateEndpointRequest,
};

pub const NEXUS_VOLUME_NAME: &str = "portal-nexus-models";
pub const NEXUS_IMAGE_ENDPOINT_NAME: &str = "portal-nexus-flux2";
pub const FLUX_WORKER_IMAGE: &str = "ghcr.io/jakob-plappert/portal-comfy-worker:0.6.0";
const MINIMUM_FLUX_VRAM_GB: u32 = 48;
const MINIMUM_FLUX_VOLUME_GB: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GpuPolicy {
    Economy,
    #[default]
    Balanced,
    Performance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProvisioningDiscovery {
    pub endpoints: Vec<Endpoint>,
    pub volumes: Vec<NetworkVolume>,
    pub gpus: Vec<GpuType>,
    pub data_centers: Vec<DataCenter>,
    pub recent_billing: BillingSummary,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectedGpu {
    pub type_id: String,
    pub display_name: String,
    pub pool_id: String,
    pub memory_gb: u32,
    /// Current provider-advertised rate, not an invoice or balance.
    pub advertised_serverless_usd_per_hour: f64,
    pub data_center_id: String,
    pub availability: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProvisioningPlan {
    pub actions: Vec<ProvisioningAction>,
    pub selected_gpu: SelectedGpu,
    pub volume_size_gb: u32,
    /// Provider billing is actual historical spend. It must never be labeled
    /// as the user's balance, which REST API v2 does not expose.
    pub actual_recent_spend_usd: Option<f64>,
    pub cost_note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProvisioningAction {
    ReuseNetworkVolume {
        id: String,
        name: String,
        size_gb: u32,
        data_center_id: String,
    },
    CreateNetworkVolume(CreateNetworkVolumeRequest),
    ReuseImageEndpoint {
        id: String,
        name: String,
    },
    CreateImageEndpoint,
    UpdateImageEndpoint {
        id: String,
    },
    VerifyEndpoint,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProvisioningResult {
    pub network_volume: NetworkVolume,
    pub image_endpoint: Endpoint,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProvisioningState {
    pub schema_version: u32,
    pub network_volume_id: Option<String>,
    pub network_volume_name: Option<String>,
    pub network_volume_size_gb: Option<u32>,
    pub data_center_id: Option<String>,
    pub image_endpoint_id: Option<String>,
    pub image_endpoint_name: Option<String>,
    pub gpu_type_id: Option<String>,
    pub gpu_pool_id: Option<String>,
    pub worker_image: Option<String>,
    pub advertised_gpu_usd_per_hour: Option<f64>,
    pub last_verified_unix: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ProvisioningStateStore {
    path: PathBuf,
}

impl ProvisioningStateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<ProvisioningState, String> {
        if !self.path.exists() {
            return Ok(ProvisioningState {
                schema_version: 1,
                ..ProvisioningState::default()
            });
        }
        let text = fs::read_to_string(&self.path)
            .map_err(|error| format!("Could not read provisioning state: {error}"))?;
        let state: ProvisioningState = toml::from_str(&text)
            .map_err(|error| format!("Could not parse provisioning state: {error}"))?;
        if state.schema_version != 1 {
            return Err(format!(
                "Unsupported Nexus provisioning schema version {}.",
                state.schema_version
            ));
        }
        Ok(state)
    }

    pub fn save(&self, state: &ProvisioningState) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| String::from("Provisioning state path has no parent."))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create provisioning directory: {error}"))?;
        let text = toml::to_string_pretty(state)
            .map_err(|error| format!("Could not serialize provisioning state: {error}"))?;
        fs::write(&self.path, text)
            .map_err(|error| format!("Could not save provisioning state: {error}"))
    }
}

pub fn discover(client: &RunPodInfrastructureClient) -> Result<ProvisioningDiscovery, String> {
    // These reads are deliberately sequential. The infrastructure worker owns
    // this entire operation off the UI thread, and avoiding nested threads
    // keeps both failure ordering and rate-limit behavior easy to understand.
    client.validate_api_key()?;
    Ok(ProvisioningDiscovery {
        endpoints: client.list_endpoints()?,
        volumes: client.list_network_volumes()?,
        gpus: client.list_gpus()?,
        data_centers: client.list_data_centers()?,
        recent_billing: client.recent_billing()?,
    })
}

pub fn build_plan(
    discovery: &ProvisioningDiscovery,
    remembered: &ProvisioningState,
    policy: GpuPolicy,
    volume_size_gb: u32,
) -> Result<ProvisioningPlan, String> {
    if !(MINIMUM_FLUX_VOLUME_GB..=4096).contains(&volume_size_gb) {
        return Err(format!(
            "FLUX.2 Network Volume size must be between {MINIMUM_FLUX_VOLUME_GB} and 4096 GB."
        ));
    }
    // A remembered ID is useful only after discovery proves it still exists.
    // A stale ID falls through to the stable Portal-owned name, which makes a
    // reinstall or manually deleted resource recoverable without duplication.
    let remembered_volume = remembered
        .network_volume_id
        .as_deref()
        .and_then(|id| discovery.volumes.iter().find(|volume| volume.id == id));
    let named_volumes = discovery
        .volumes
        .iter()
        .filter(|volume| volume.name == NEXUS_VOLUME_NAME)
        .collect::<Vec<_>>();
    if remembered_volume.is_none() && named_volumes.len() > 1 {
        return Err(format!(
            "RunPod has multiple Network Volumes named '{NEXUS_VOLUME_NAME}'. Nexus will not choose one automatically; resolve the duplicate names in RunPod first."
        ));
    }
    let volume = remembered_volume.or_else(|| named_volumes.first().copied());
    let selected_gpu = select_gpu(
        discovery,
        policy,
        volume.map(|volume| volume.data_center.as_str()),
    )?;

    let mut actions = Vec::new();
    if let Some(volume) = volume {
        if volume.size < MINIMUM_FLUX_VOLUME_GB {
            return Err(format!(
                "Existing volume '{}' is {} GB; FLUX.2 bootstrap requires at least {} GB. Nexus will not create a duplicate automatically.",
                volume.name, volume.size, MINIMUM_FLUX_VOLUME_GB
            ));
        }
        if volume.data_center != selected_gpu.data_center_id {
            return Err(format!(
                "Existing volume '{}' is in {}, but no compatible selected GPU was found there. Choose another GPU policy or manage the old volume in RunPod.",
                volume.name, volume.data_center
            ));
        }
        actions.push(ProvisioningAction::ReuseNetworkVolume {
            id: volume.id.clone(),
            name: volume.name.clone(),
            size_gb: volume.size,
            data_center_id: volume.data_center.clone(),
        });
    } else {
        actions.push(ProvisioningAction::CreateNetworkVolume(
            CreateNetworkVolumeRequest {
                name: String::from(NEXUS_VOLUME_NAME),
                size: volume_size_gb,
                data_center: selected_gpu.data_center_id.clone(),
                volume_type: String::from("STANDARD"),
            },
        ));
    }

    let remembered_endpoint = remembered.image_endpoint_id.as_deref().and_then(|id| {
        discovery
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == id)
    });
    let named_endpoints = discovery
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.name == NEXUS_IMAGE_ENDPOINT_NAME)
        .collect::<Vec<_>>();
    if remembered_endpoint.is_none() && named_endpoints.len() > 1 {
        return Err(format!(
            "RunPod has multiple endpoints named '{NEXUS_IMAGE_ENDPOINT_NAME}'. Nexus will not choose one automatically; resolve the duplicate names in RunPod first."
        ));
    }
    let endpoint = remembered_endpoint.or_else(|| named_endpoints.first().copied());
    match endpoint {
        Some(endpoint)
            if endpoint_matches(endpoint, volume.map(|item| item.id.as_str()), &selected_gpu) =>
        {
            actions.push(ProvisioningAction::ReuseImageEndpoint {
                id: endpoint.id.clone(),
                name: endpoint.name.clone(),
            });
        }
        Some(endpoint) => actions.push(ProvisioningAction::UpdateImageEndpoint {
            id: endpoint.id.clone(),
        }),
        None => actions.push(ProvisioningAction::CreateImageEndpoint),
    }
    actions.push(ProvisioningAction::VerifyEndpoint);

    Ok(ProvisioningPlan {
        actions,
        selected_gpu,
        volume_size_gb,
        actual_recent_spend_usd: discovery.recent_billing.total_usd,
        cost_note: String::from(
            "Estimated future compute is the advertised hourly rate × execution time. Idle compute is $0 with min workers 0; persistent Network Volume storage is still billable.",
        ),
    })
}

pub fn apply_plan(
    client: &RunPodInfrastructureClient,
    state_store: &ProvisioningStateStore,
    plan: &ProvisioningPlan,
) -> Result<ProvisioningResult, String> {
    let mut remembered = state_store.load()?;
    let mut volume: Option<NetworkVolume> = None;
    let mut endpoint: Option<Endpoint> = None;
    let mut messages = Vec::new();

    for action in &plan.actions {
        match action {
            ProvisioningAction::ReuseNetworkVolume { id, .. } => {
                volume = Some(client.get_network_volume(id)?);
                messages.push(String::from("Reused existing Network Volume."));
            }
            ProvisioningAction::CreateNetworkVolume(request) => {
                let created = client.create_network_volume(request)?;
                // Save immediately. If endpoint creation fails, retrying later
                // reuses this billable resource instead of creating another.
                remembered.network_volume_id = Some(created.id.clone());
                remembered.network_volume_name = Some(created.name.clone());
                remembered.network_volume_size_gb = Some(created.size);
                remembered.data_center_id = Some(created.data_center.clone());
                remembered.schema_version = 1;
                state_store.save(&remembered).map_err(|error| {
                    format!(
                        "Network Volume was created, but Nexus could not save its local ID. Refresh will rediscover it by name; do not create another volume manually. {error}"
                    )
                })?;
                volume = Some(created);
                messages.push(String::from("Created Network Volume."));
            }
            ProvisioningAction::ReuseImageEndpoint { id, .. } => {
                endpoint = Some(client.get_endpoint(id)?);
                messages.push(String::from("Reused existing FLUX.2 endpoint."));
            }
            ProvisioningAction::CreateImageEndpoint => {
                let volume_id = volume
                    .as_ref()
                    .map(|volume| volume.id.as_str())
                    .ok_or_else(|| {
                        String::from("Cannot create endpoint before its volume exists.")
                    })?;
                let created = client
                    .create_endpoint(&endpoint_create_request(&plan.selected_gpu, volume_id))
                    .map_err(|error| endpoint_failure_message(&messages, error))?;
                // Persist the endpoint before verification for the same
                // partial-success reason as the volume above. A retry can
                // verify or update it instead of creating a duplicate.
                remembered.image_endpoint_id = Some(created.id.clone());
                remembered.image_endpoint_name = Some(created.name.clone());
                remembered.schema_version = 1;
                state_store.save(&remembered).map_err(|error| {
                    format!(
                        "The FLUX.2 endpoint was created, but Nexus could not save its local ID. Refresh will rediscover it by name. {error}"
                    )
                })?;
                endpoint = Some(created);
                messages.push(String::from("Created FLUX.2 Serverless endpoint."));
            }
            ProvisioningAction::UpdateImageEndpoint { id } => {
                let volume_id = volume
                    .as_ref()
                    .map(|volume| volume.id.as_str())
                    .ok_or_else(|| {
                        String::from("Cannot update endpoint before its volume exists.")
                    })?;
                let updated = client
                    .update_endpoint(id, &endpoint_update_request(&plan.selected_gpu, volume_id))
                    .map_err(|error| endpoint_failure_message(&messages, error))?;
                remembered.image_endpoint_id = Some(updated.id.clone());
                remembered.image_endpoint_name = Some(updated.name.clone());
                remembered.schema_version = 1;
                state_store.save(&remembered).map_err(|error| {
                    format!(
                        "The FLUX.2 endpoint was updated, but Nexus could not save its local ID. Refresh will rediscover it by name. {error}"
                    )
                })?;
                endpoint = Some(updated);
                messages.push(String::from("Updated existing FLUX.2 endpoint."));
            }
            ProvisioningAction::VerifyEndpoint => {
                let id = endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.id.as_str())
                    .ok_or_else(|| {
                        String::from("Cannot verify an endpoint that does not exist.")
                    })?;
                endpoint = Some(client.get_endpoint(id)?);
                messages.push(String::from(
                    "Verified endpoint through RunPod REST API v2.",
                ));
            }
        }
    }

    let volume = volume.ok_or_else(|| String::from("Provisioning produced no Network Volume."))?;
    let endpoint = endpoint.ok_or_else(|| String::from("Provisioning produced no endpoint."))?;
    remembered.schema_version = 1;
    remembered.network_volume_id = Some(volume.id.clone());
    remembered.network_volume_name = Some(volume.name.clone());
    remembered.network_volume_size_gb = Some(volume.size);
    remembered.data_center_id = Some(volume.data_center.clone());
    remembered.image_endpoint_id = Some(endpoint.id.clone());
    remembered.image_endpoint_name = Some(endpoint.name.clone());
    remembered.gpu_type_id = Some(plan.selected_gpu.type_id.clone());
    remembered.gpu_pool_id = Some(plan.selected_gpu.pool_id.clone());
    remembered.worker_image = Some(String::from(FLUX_WORKER_IMAGE));
    remembered.advertised_gpu_usd_per_hour =
        Some(plan.selected_gpu.advertised_serverless_usd_per_hour);
    remembered.last_verified_unix = Some(now_unix()?);
    state_store.save(&remembered)?;

    Ok(ProvisioningResult {
        network_volume: volume,
        image_endpoint: endpoint,
        messages,
    })
}

pub fn endpoint_create_request(gpu: &SelectedGpu, volume_id: &str) -> CreateEndpointRequest {
    CreateEndpointRequest {
        name: String::from(NEXUS_IMAGE_ENDPOINT_NAME),
        image: String::from(FLUX_WORKER_IMAGE),
        endpoint_type: String::from("QUEUE"),
        gpu: endpoint_gpu(gpu),
        workers: endpoint_workers(),
        scaling: endpoint_scaling(),
        data_center_ids: vec![gpu.data_center_id.clone()],
        network_volumes: vec![volume_id.to_string()],
        // The first worker may download roughly 54 GB of model data. One hour
        // gives slow first-time bootstrap room; ordinary warm inference should
        // be much shorter and workers still scale to zero when idle.
        timeout: 3_600_000,
        disk: 20,
    }
}

fn endpoint_failure_message(messages: &[String], error: String) -> String {
    if messages
        .iter()
        .any(|message| message == "Created Network Volume.")
    {
        format!(
            "Network Volume was created, but endpoint provisioning failed. Retry will reuse the existing volume. {error}"
        )
    } else {
        error
    }
}

fn endpoint_update_request(gpu: &SelectedGpu, volume_id: &str) -> UpdateEndpointRequest {
    let create = endpoint_create_request(gpu, volume_id);
    UpdateEndpointRequest {
        image: create.image,
        gpu: create.gpu,
        workers: create.workers,
        scaling: create.scaling,
        data_center_ids: create.data_center_ids,
        network_volumes: create.network_volumes,
        timeout: create.timeout,
        disk: create.disk,
    }
}

fn endpoint_gpu(gpu: &SelectedGpu) -> EndpointGpu {
    EndpointGpu {
        pools: vec![gpu.pool_id.clone()],
        count: 1,
    }
}

fn endpoint_workers() -> EndpointWorkers {
    EndpointWorkers {
        min: 0,
        max: 1,
        idle_timeout: 5,
    }
}

fn endpoint_scaling() -> EndpointScaling {
    EndpointScaling {
        scaling_type: String::from("QUEUE_DELAY"),
        queue_delay: 4,
    }
}

fn endpoint_matches(endpoint: &Endpoint, volume_id: Option<&str>, gpu: &SelectedGpu) -> bool {
    endpoint.name == NEXUS_IMAGE_ENDPOINT_NAME
        && endpoint.image == FLUX_WORKER_IMAGE
        && endpoint.workers.min == 0
        && endpoint.workers.max == 1
        && endpoint.timeout == 3_600_000
        && endpoint.disk == 20
        && endpoint
            .data_center_ids
            .iter()
            .any(|id| id == &gpu.data_center_id)
        && endpoint
            .gpu
            .as_ref()
            .is_some_and(|configured| configured.pools.contains(&gpu.pool_id))
        && volume_id.is_some_and(|id| endpoint.network_volumes.iter().any(|item| item == id))
}

fn select_gpu(
    discovery: &ProvisioningDiscovery,
    policy: GpuPolicy,
    preferred_data_center: Option<&str>,
) -> Result<SelectedGpu, String> {
    let mut candidates = discovery
        .gpus
        .iter()
        .filter_map(|gpu| {
            let pool_id = gpu.pool.as_ref()?;
            let rate = gpu.price.serverless?;
            if gpu.memory < MINIMUM_FLUX_VRAM_GB || gpu.availability.as_deref() == Some("NONE") {
                return None;
            }
            let location = gpu
                .data_centers
                .iter()
                .filter(|gpu_dc| {
                    preferred_data_center.is_none_or(|preferred| gpu_dc.id == preferred)
                        && gpu_dc.availability != "NONE"
                        && discovery.data_centers.iter().any(|data_center| {
                            data_center.id == gpu_dc.id
                                && data_center
                                    .network_volume_types
                                    .iter()
                                    .any(|kind| kind == "STANDARD")
                        })
                })
                .max_by_key(|gpu_dc| availability_rank(&gpu_dc.availability))?;
            Some(SelectedGpu {
                type_id: gpu.id.clone(),
                display_name: gpu.name.clone(),
                pool_id: pool_id.clone(),
                memory_gb: gpu.memory,
                advertised_serverless_usd_per_hour: rate,
                data_center_id: location.id.clone(),
                availability: location.availability.clone(),
            })
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| match policy {
        GpuPolicy::Economy => rate_order(left, right),
        GpuPolicy::Balanced => availability_rank(&right.availability)
            .cmp(&availability_rank(&left.availability))
            .then_with(|| rate_order(left, right)),
        GpuPolicy::Performance => right
            .memory_gb
            .cmp(&left.memory_gb)
            .then_with(|| rate_order(left, right)),
    });
    candidates.into_iter().next().ok_or_else(|| {
        String::from(
            "No currently available Serverless GPU with at least 48 GB VRAM shares a data center with STANDARD Network Volume support.",
        )
    })
}

fn rate_order(left: &SelectedGpu, right: &SelectedGpu) -> Ordering {
    left.advertised_serverless_usd_per_hour
        .partial_cmp(&right.advertised_serverless_usd_per_hour)
        .unwrap_or(Ordering::Equal)
}

fn availability_rank(value: &str) -> u8 {
    match value {
        "HIGH" => 3,
        "MEDIUM" => 2,
        "LOW" => 1,
        _ => 0,
    }
}

fn now_unix() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("System clock is before the Unix epoch: {error}"))
}

// No `Debug`: commands temporarily carry the RunPod key across the channel.
#[derive(Clone)]
pub enum InfrastructureCommand {
    Validate {
        api_key: String,
    },
    DiscoverAndPlan {
        api_key: String,
        policy: GpuPolicy,
        volume_size_gb: u32,
    },
    ApplyConfirmed {
        api_key: String,
    },
}

#[derive(Debug, Clone)]
pub enum InfrastructureEvent {
    Connected,
    PlanReady(ProvisioningPlan),
    Applied(ProvisioningResult),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct InfrastructureWorker {
    sender: Sender<InfrastructureCommand>,
}

impl InfrastructureWorker {
    pub fn start(
        state_store: ProvisioningStateStore,
        on_event: impl Fn(InfrastructureEvent) + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();

        // All REST reads and billable writes happen away from Slint's event
        // loop. Owned commands satisfy `Send + 'static`; no UI reference is
        // borrowed by the thread. `latest_plan` is private to this worker and
        // proves Apply can only follow an explicit, successfully shown plan.
        thread::spawn(move || {
            let mut latest_plan: Option<ProvisioningPlan> = None;
            while let Ok(command) = receiver.recv() {
                match command {
                    InfrastructureCommand::Validate { api_key } => {
                        let result = RunPodInfrastructureClient::new(api_key)
                            .and_then(|client| client.validate_api_key());
                        match result {
                            Ok(()) => on_event(InfrastructureEvent::Connected),
                            Err(error) => on_event(InfrastructureEvent::Failed(error)),
                        }
                    }
                    InfrastructureCommand::DiscoverAndPlan {
                        api_key,
                        policy,
                        volume_size_gb,
                    } => {
                        let result = RunPodInfrastructureClient::new(api_key).and_then(|client| {
                            let discovery = discover(&client)?;
                            let remembered = state_store.load()?;
                            build_plan(&discovery, &remembered, policy, volume_size_gb)
                        });
                        match result {
                            Ok(plan) => {
                                latest_plan = Some(plan.clone());
                                on_event(InfrastructureEvent::PlanReady(plan));
                            }
                            Err(error) => on_event(InfrastructureEvent::Failed(error)),
                        }
                    }
                    InfrastructureCommand::ApplyConfirmed { api_key } => {
                        let result = latest_plan
                            .as_ref()
                            .ok_or_else(|| {
                                String::from(
                                    "Create Infrastructure requires a current discovery plan.",
                                )
                            })
                            .and_then(|plan| {
                                let client = RunPodInfrastructureClient::new(api_key)?;
                                apply_plan(&client, &state_store, plan)
                            });
                        match result {
                            Ok(result) => {
                                latest_plan = None;
                                on_event(InfrastructureEvent::Applied(result));
                            }
                            Err(error) => on_event(InfrastructureEvent::Failed(error)),
                        }
                    }
                }
            }
        });
        Self { sender }
    }

    pub fn send(&self, command: InfrastructureCommand) -> Result<(), String> {
        self.sender
            .send(command)
            .map_err(|_| String::from("RunPod infrastructure worker has stopped."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runpod_infrastructure::{GpuDataCenter, GpuPrice};

    fn discovery() -> ProvisioningDiscovery {
        ProvisioningDiscovery {
            endpoints: Vec::new(),
            volumes: Vec::new(),
            gpus: vec![GpuType {
                id: String::from("NVIDIA L40S"),
                name: String::from("L40S"),
                pool: Some(String::from("ADA_48")),
                memory: 48,
                price: GpuPrice {
                    serverless: Some(1.25),
                },
                availability: Some(String::from("HIGH")),
                data_centers: vec![GpuDataCenter {
                    id: String::from("EU-1"),
                    name: String::from("Europe"),
                    availability: String::from("HIGH"),
                }],
            }],
            data_centers: vec![DataCenter {
                id: String::from("EU-1"),
                name: String::from("Europe"),
                network_volume_types: vec![String::from("STANDARD")],
            }],
            recent_billing: BillingSummary {
                total_usd: Some(4.5),
            },
        }
    }

    fn volume() -> NetworkVolume {
        NetworkVolume {
            id: String::from("volume-live"),
            name: String::from(NEXUS_VOLUME_NAME),
            size: 150,
            data_center: String::from("EU-1"),
            volume_type: String::from("STANDARD"),
        }
    }

    fn endpoint(volume_id: &str) -> Endpoint {
        Endpoint {
            id: String::from("endpoint-live"),
            name: String::from(NEXUS_IMAGE_ENDPOINT_NAME),
            image: String::from(FLUX_WORKER_IMAGE),
            workers: endpoint_workers(),
            data_center_ids: vec![String::from("EU-1")],
            network_volumes: vec![volume_id.to_string()],
            gpu: Some(EndpointGpu {
                pools: vec![String::from("ADA_48")],
                count: 1,
            }),
            timeout: 3_600_000,
            disk: 20,
        }
    }

    #[test]
    fn first_plan_describes_creates_but_does_not_apply_them() {
        let plan = build_plan(
            &discovery(),
            &ProvisioningState::default(),
            GpuPolicy::Balanced,
            150,
        )
        .expect("plan should build from discovery only");
        assert!(matches!(
            plan.actions[0],
            ProvisioningAction::CreateNetworkVolume(_)
        ));
        assert!(matches!(
            plan.actions[1],
            ProvisioningAction::CreateImageEndpoint
        ));
        assert!(plan.cost_note.contains("Estimated future compute"));
    }

    #[test]
    fn matching_resources_are_reused_idempotently() {
        let mut found = discovery();
        found.volumes.push(volume());
        found.endpoints.push(endpoint("volume-live"));
        let plan = build_plan(
            &found,
            &ProvisioningState::default(),
            GpuPolicy::Balanced,
            150,
        )
        .expect("matching resources should plan");
        assert!(matches!(
            plan.actions[0],
            ProvisioningAction::ReuseNetworkVolume { .. }
        ));
        assert!(matches!(
            plan.actions[1],
            ProvisioningAction::ReuseImageEndpoint { .. }
        ));
    }

    #[test]
    fn stale_remembered_ids_are_rediscovered_by_name() {
        let mut found = discovery();
        found.volumes.push(volume());
        found.endpoints.push(endpoint("volume-live"));
        let remembered = ProvisioningState {
            schema_version: 1,
            network_volume_id: Some(String::from("deleted-volume")),
            image_endpoint_id: Some(String::from("deleted-endpoint")),
            ..ProvisioningState::default()
        };
        let plan = build_plan(&found, &remembered, GpuPolicy::Balanced, 150)
            .expect("names should recover stale IDs");
        assert!(matches!(
            &plan.actions[0],
            ProvisioningAction::ReuseNetworkVolume { id, .. } if id == "volume-live"
        ));
        assert!(matches!(
            &plan.actions[1],
            ProvisioningAction::ReuseImageEndpoint { id, .. } if id == "endpoint-live"
        ));
    }

    #[test]
    fn provisioning_state_contains_no_secrets() {
        let state = ProvisioningState {
            schema_version: 1,
            image_endpoint_id: Some(String::from("endpoint")),
            ..ProvisioningState::default()
        };
        let text = toml::to_string(&state).expect("state should serialize");
        assert!(!text.contains("api_key"));
        assert!(!text.contains("hf_token"));
    }

    #[test]
    fn partial_failure_message_preserves_created_volume() {
        let message = endpoint_failure_message(
            &[String::from("Created Network Volume.")],
            String::from("endpoint error"),
        );
        assert!(message.contains("Retry will reuse"));
        assert!(message.contains("endpoint error"));
    }
}
