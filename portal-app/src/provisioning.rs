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
use crate::setup::{PreflightReport, run_preflight};
use crate::{paths::PortalPaths, storage::NexusStore};

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
    /// REST API v2 currently does not publish a Serverless hourly rate. This
    /// separate `Option` prevents a Pod catalog price from being mistaken for
    /// the eventual endpoint's compute rate.
    pub serverless_usd_per_hour: Option<f64>,
    pub secure_pod_reference_usd_per_hour: f64,
    pub community_pod_reference_usd_per_hour: f64,
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
    pub serverless_usd_per_hour: Option<f64>,
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
            "Serverless live rate is unavailable through the current REST v2 response used by Portal, so future compute cost is not estimated. Min workers 0 avoids continuously running workers, but does not make the setup free: jobs incur compute charges and the persistent Network Volume is billable while it exists.",
        ),
    })
}

fn apply_plan_with_progress(
    client: &RunPodInfrastructureClient,
    state_store: &ProvisioningStateStore,
    plan: &ProvisioningPlan,
    mut on_progress: impl FnMut(String),
) -> Result<ProvisioningResult, String> {
    let mut remembered = state_store.load()?;
    let mut volume: Option<NetworkVolume> = None;
    let mut endpoint: Option<Endpoint> = None;
    let mut messages = Vec::new();

    for action in &plan.actions {
        match action {
            ProvisioningAction::ReuseNetworkVolume { id, .. } => {
                on_progress(String::from("• Creating/reusing Network Volume"));
                let reused = client.get_network_volume(id)?;
                on_progress(String::from("✓ Network Volume reused"));
                on_progress(String::from("• Saving Network Volume identity"));
                remembered.network_volume_id = Some(reused.id.clone());
                remembered.network_volume_name = Some(reused.name.clone());
                remembered.network_volume_size_gb = Some(reused.size);
                remembered.data_center_id = Some(reused.data_center.clone());
                remembered.schema_version = 1;
                state_store.save(&remembered)?;
                on_progress(String::from("✓ Network Volume identity saved"));
                volume = Some(reused);
                messages.push(String::from("Reused existing Network Volume."));
            }
            ProvisioningAction::CreateNetworkVolume(request) => {
                on_progress(String::from("• Creating/reusing Network Volume"));
                let created = client.create_network_volume(request)?;
                on_progress(String::from("✓ Network Volume created"));
                on_progress(String::from("• Saving Network Volume identity"));
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
                on_progress(String::from("✓ Network Volume identity saved"));
                volume = Some(created);
                messages.push(String::from("Created Network Volume."));
            }
            ProvisioningAction::ReuseImageEndpoint { id, .. } => {
                on_progress(String::from("• Creating/updating Serverless endpoint"));
                let reused = client.get_endpoint(id)?;
                on_progress(String::from("✓ Serverless endpoint reused"));
                on_progress(String::from("• Saving endpoint identity"));
                remembered.image_endpoint_id = Some(reused.id.clone());
                remembered.image_endpoint_name = Some(reused.name.clone());
                remembered.schema_version = 1;
                state_store.save(&remembered)?;
                on_progress(String::from("✓ Endpoint identity saved"));
                endpoint = Some(reused);
                messages.push(String::from("Reused existing FLUX.2 endpoint."));
            }
            ProvisioningAction::CreateImageEndpoint => {
                on_progress(String::from("• Creating/updating Serverless endpoint"));
                let volume_id = volume
                    .as_ref()
                    .map(|volume| volume.id.as_str())
                    .ok_or_else(|| {
                        String::from("Cannot create endpoint before its volume exists.")
                    })?;
                let created = client
                    .create_endpoint(&endpoint_create_request(&plan.selected_gpu, volume_id))
                    .map_err(|error| endpoint_failure_message(&messages, error))?;
                on_progress(String::from("✓ Serverless endpoint created"));
                on_progress(String::from("• Saving endpoint identity"));
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
                on_progress(String::from("✓ Endpoint identity saved"));
                endpoint = Some(created);
                messages.push(String::from("Created FLUX.2 Serverless endpoint."));
            }
            ProvisioningAction::UpdateImageEndpoint { id } => {
                on_progress(String::from("• Creating/updating Serverless endpoint"));
                let volume_id = volume
                    .as_ref()
                    .map(|volume| volume.id.as_str())
                    .ok_or_else(|| {
                        String::from("Cannot update endpoint before its volume exists.")
                    })?;
                let updated = client
                    .update_endpoint(id, &endpoint_update_request(&plan.selected_gpu, volume_id))
                    .map_err(|error| endpoint_failure_message(&messages, error))?;
                on_progress(String::from("✓ Serverless endpoint updated"));
                on_progress(String::from("• Saving endpoint identity"));
                remembered.image_endpoint_id = Some(updated.id.clone());
                remembered.image_endpoint_name = Some(updated.name.clone());
                remembered.schema_version = 1;
                state_store.save(&remembered).map_err(|error| {
                    format!(
                        "The FLUX.2 endpoint was updated, but Nexus could not save its local ID. Refresh will rediscover it by name. {error}"
                    )
                })?;
                on_progress(String::from("✓ Endpoint identity saved"));
                endpoint = Some(updated);
                messages.push(String::from("Updated existing FLUX.2 endpoint."));
            }
            ProvisioningAction::VerifyEndpoint => {
                on_progress(String::from("• Verifying endpoint configuration"));
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
                on_progress(String::from("✓ Endpoint configuration verified"));
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
    remembered.serverless_usd_per_hour = plan.selected_gpu.serverless_usd_per_hour;
    remembered.last_verified_unix = Some(now_unix()?);
    state_store.save(&remembered)?;
    on_progress(String::from("✓ Finished"));

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
                serverless_usd_per_hour: None,
                secure_pod_reference_usd_per_hour: gpu.price.secure,
                community_pod_reference_usd_per_hour: gpu.price.community,
                data_center_id: location.id.clone(),
                availability: location.availability.clone(),
            })
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| match policy {
        GpuPolicy::Economy => pod_reference_rate_order(left, right).then_with(|| {
            availability_rank(&right.availability).cmp(&availability_rank(&left.availability))
        }),
        GpuPolicy::Balanced => availability_rank(&right.availability)
            .cmp(&availability_rank(&left.availability))
            .then_with(|| pod_reference_rate_order(left, right)),
        GpuPolicy::Performance => right
            .memory_gb
            .cmp(&left.memory_gb)
            .then_with(|| {
                availability_rank(&right.availability).cmp(&availability_rank(&left.availability))
            })
            .then_with(|| pod_reference_rate_order(left, right)),
    });
    candidates.into_iter().next().ok_or_else(|| {
        String::from(
            "No currently available Serverless GPU with at least 48 GB VRAM shares a data center with STANDARD Network Volume support.",
        )
    })
}

fn pod_reference_rate_order(left: &SelectedGpu, right: &SelectedGpu) -> Ordering {
    // This is only a deterministic selection heuristic. Secure Pod catalog
    // pricing is not used for a Serverless cost claim or job estimate.
    left.secure_pod_reference_usd_per_hour
        .partial_cmp(&right.secure_pod_reference_usd_per_hour)
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

fn reusable_resources<'a>(
    plan: &ProvisioningPlan,
    discovery: &'a ProvisioningDiscovery,
) -> Option<(&'a NetworkVolume, &'a Endpoint)> {
    let (volume_id, endpoint_id) = match plan.actions.as_slice() {
        [
            ProvisioningAction::ReuseNetworkVolume { id: volume_id, .. },
            ProvisioningAction::ReuseImageEndpoint {
                id: endpoint_id, ..
            },
            ProvisioningAction::VerifyEndpoint,
        ] => (volume_id, endpoint_id),
        _ => return None,
    };
    Some((
        discovery
            .volumes
            .iter()
            .find(|volume| volume.id == *volume_id)?,
        discovery
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == *endpoint_id)?,
    ))
}

fn remember_verified_resources(
    state_store: &ProvisioningStateStore,
    plan: &ProvisioningPlan,
    volume: &NetworkVolume,
    endpoint: &Endpoint,
) -> Result<(), String> {
    // Rediscovery is read-only at RunPod. Saving the IDs locally repairs stale
    // state after reinstall or manual provider changes and prevents the setup
    // UI from pretending valid infrastructure is incomplete after restart.
    let mut remembered = state_store.load()?;
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
    remembered.serverless_usd_per_hour = plan.selected_gpu.serverless_usd_per_hour;
    remembered.last_verified_unix = Some(now_unix()?);
    state_store.save(&remembered)
}

// No `Debug`: commands temporarily carry the RunPod key across the channel.
#[derive(Clone)]
pub enum InfrastructureCommand {
    RunPreflight {
        api_key: String,
    },
    PlanInfrastructure {
        policy: GpuPolicy,
        volume_size_gb: u32,
    },
    ApplyConfirmed {
        api_key: String,
    },
}

#[derive(Debug, Clone)]
pub enum InfrastructureEvent {
    PreflightFinished {
        report: PreflightReport,
        infrastructure_ready: bool,
        ready_volume: Option<NetworkVolume>,
        ready_endpoint: Option<Endpoint>,
    },
    PlanReady(ProvisioningPlan),
    Progress(String),
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
        paths: PortalPaths,
        store: NexusStore,
        on_event: impl Fn(InfrastructureEvent) + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();

        // All REST reads and billable writes happen away from Slint's event
        // loop. Owned commands satisfy `Send + 'static`; no UI reference is
        // borrowed by the thread. `latest_plan` is private to this worker and
        // proves Apply can only follow an explicit, successfully shown plan.
        thread::spawn(move || {
            let mut latest_plan: Option<ProvisioningPlan> = None;
            let mut latest_discovery: Option<ProvisioningDiscovery> = None;
            while let Ok(command) = receiver.recv() {
                match command {
                    InfrastructureCommand::RunPreflight { api_key } => {
                        latest_plan = None;
                        let outcome = run_preflight(&paths, &store, &api_key);
                        let mut ready_volume = None;
                        let mut ready_endpoint = None;
                        if let Some(discovery) = outcome.discovery.as_ref()
                            && let Ok(remembered) = state_store.load()
                            && let Ok(plan) =
                                build_plan(discovery, &remembered, GpuPolicy::Balanced, 150)
                            && let Some((volume, endpoint)) = reusable_resources(&plan, discovery)
                            && remember_verified_resources(&state_store, &plan, volume, endpoint)
                                .is_ok()
                        {
                            ready_volume = Some(volume.clone());
                            ready_endpoint = Some(endpoint.clone());
                        }
                        latest_discovery = outcome.discovery;
                        on_event(InfrastructureEvent::PreflightFinished {
                            report: outcome.report,
                            infrastructure_ready: ready_endpoint.is_some(),
                            ready_volume,
                            ready_endpoint,
                        });
                    }
                    InfrastructureCommand::PlanInfrastructure {
                        policy,
                        volume_size_gb,
                    } => {
                        let result = (|| {
                            let discovery = latest_discovery.as_ref().ok_or_else(|| {
                                String::from(
                                    "Run Preflight successfully before planning infrastructure.",
                                )
                            })?;
                            let remembered = state_store.load()?;
                            build_plan(discovery, &remembered, policy, volume_size_gb)
                        })();
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
                                apply_plan_with_progress(&client, &state_store, plan, |message| {
                                    on_event(InfrastructureEvent::Progress(message));
                                })
                            });
                        match result {
                            Ok(result) => {
                                latest_plan = None;
                                if let Some(discovery) = latest_discovery.as_mut() {
                                    discovery.volumes.retain(|volume| {
                                        volume.id != result.network_volume.id
                                            && volume.name != result.network_volume.name
                                    });
                                    discovery.volumes.push(result.network_volume.clone());
                                    discovery.endpoints.retain(|endpoint| {
                                        endpoint.id != result.image_endpoint.id
                                            && endpoint.name != result.image_endpoint.name
                                    });
                                    discovery.endpoints.push(result.image_endpoint.clone());
                                }
                                on_event(InfrastructureEvent::Applied(result));
                            }
                            Err(error) => {
                                // The provider may have completed an early
                                // durable stage (most importantly volume
                                // creation). The old discovery snapshot cannot
                                // see that resource, so it must never be reused
                                // to plan a retry. A fresh preflight will find
                                // the saved/name-matched volume and reuse it.
                                latest_plan = None;
                                latest_discovery = None;
                                on_event(InfrastructureEvent::Failed(format!(
                                    "{error} Run Preflight again before retrying so Nexus can rediscover every durable resource."
                                )));
                            }
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
                    secure: 0.8,
                    community: 0.6,
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
        assert!(
            plan.cost_note
                .contains("Serverless live rate is unavailable")
        );
        assert_eq!(plan.selected_gpu.serverless_usd_per_hour, None);
        assert_eq!(plan.selected_gpu.secure_pod_reference_usd_per_hour, 0.8);
    }

    #[test]
    fn gpu_selection_succeeds_without_a_serverless_price() {
        for policy in [
            GpuPolicy::Economy,
            GpuPolicy::Balanced,
            GpuPolicy::Performance,
        ] {
            let plan = build_plan(&discovery(), &ProvisioningState::default(), policy, 150)
                .expect("documented Pod reference prices must not block Serverless GPU selection");

            assert_eq!(plan.selected_gpu.type_id, "NVIDIA L40S");
            assert_eq!(plan.selected_gpu.serverless_usd_per_hour, None);
            assert_eq!(plan.selected_gpu.community_pod_reference_usd_per_hour, 0.6);
        }
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
    fn restart_rediscovery_repairs_stale_durable_ids() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let state_store = ProvisioningStateStore::new(directory.path().join("infra.toml"));
        state_store
            .save(&ProvisioningState {
                schema_version: 1,
                network_volume_id: Some(String::from("deleted-volume")),
                image_endpoint_id: Some(String::from("deleted-endpoint")),
                ..ProvisioningState::default()
            })
            .expect("stale state should save");
        let mut found = discovery();
        found.volumes.push(volume());
        found.endpoints.push(endpoint("volume-live"));
        let plan = build_plan(
            &found,
            &state_store.load().expect("state should load"),
            GpuPolicy::Balanced,
            150,
        )
        .expect("rediscovered resources should plan");
        let (volume, endpoint) = reusable_resources(&plan, &found)
            .expect("matching resources should be recognized as ready");
        remember_verified_resources(&state_store, &plan, volume, endpoint)
            .expect("rediscovered identity should persist");
        let repaired = state_store.load().expect("repaired state should load");
        assert_eq!(repaired.network_volume_id.as_deref(), Some("volume-live"));
        assert_eq!(repaired.image_endpoint_id.as_deref(), Some("endpoint-live"));
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
