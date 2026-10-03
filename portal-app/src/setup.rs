use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::paths::PortalPaths;
use crate::provisioning::{FLUX_WORKER_IMAGE, ProvisioningDiscovery, ProvisioningState};
use crate::registry_preflight::{PublicImageStatus, check_public_ghcr_image};
use crate::runpod_infrastructure::{BillingSummary, RunPodInfrastructureClient};
use crate::storage::NexusStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    NotConfigured,
    ApiKeyReady,
    PreflightRunning,
    PreflightFailed,
    DiscoveryReady,
    PlanReady,
    AwaitingInfrastructureConfirmation,
    Provisioning,
    InfrastructureReady,
    AwaitingTestGenerationConfirmation,
    TestGenerationRunning,
    Ready,
}

impl SetupState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NotConfigured => "Not configured",
            Self::ApiKeyReady => "API key ready",
            Self::PreflightRunning => "Preflight running",
            Self::PreflightFailed => "Preflight failed",
            Self::DiscoveryReady => "Discovery ready",
            Self::PlanReady => "Infrastructure plan ready",
            Self::AwaitingInfrastructureConfirmation => "Awaiting infrastructure confirmation",
            Self::Provisioning => "Provisioning infrastructure",
            Self::InfrastructureReady => "Infrastructure ready",
            Self::AwaitingTestGenerationConfirmation => "Awaiting test generation confirmation",
            Self::TestGenerationRunning => "Test generation running",
            Self::Ready => "Ready",
        }
    }

    pub fn ui_index(self) -> i32 {
        self as i32
    }
}

/// Transient setup state stays in memory. Durable provider IDs remain in
/// `ProvisioningState`; after restart a new preflight rediscovers and verifies
/// them instead of trusting that a previous wizard happened to finish.
#[derive(Debug, Clone)]
pub struct SetupSession {
    pub state: SetupState,
    pub diagnostics: Vec<PreflightCheck>,
    pub test_job_id: Option<String>,
    pub last_generation_state: Option<String>,
}

impl SetupSession {
    pub fn new(api_key_exists: bool) -> Self {
        Self {
            state: if api_key_exists {
                SetupState::ApiKeyReady
            } else {
                SetupState::NotConfigured
            },
            diagnostics: Vec::new(),
            test_job_id: None,
            last_generation_state: None,
        }
    }

    pub fn api_key_saved(&mut self) {
        self.state = SetupState::ApiKeyReady;
    }

    pub fn begin_preflight(&mut self) -> Result<(), String> {
        if self.state == SetupState::NotConfigured {
            return Err(String::from(
                "Save a RunPod API key before running preflight.",
            ));
        }
        self.state = SetupState::PreflightRunning;
        self.diagnostics.clear();
        Ok(())
    }

    pub fn finish_preflight(&mut self, report: PreflightReport, infrastructure_ready: bool) {
        let failed = report.has_failures();
        self.diagnostics = report.checks;
        self.state = if failed {
            SetupState::PreflightFailed
        } else if infrastructure_ready {
            SetupState::InfrastructureReady
        } else {
            SetupState::DiscoveryReady
        };
    }

    pub fn plan_ready(&mut self) -> Result<(), String> {
        if !matches!(
            self.state,
            SetupState::DiscoveryReady | SetupState::InfrastructureReady | SetupState::Ready
        ) {
            return Err(String::from(
                "Infrastructure planning requires a successful current preflight.",
            ));
        }
        self.state = SetupState::PlanReady;
        Ok(())
    }

    pub fn await_infrastructure_confirmation(&mut self) -> Result<(), String> {
        if self.state != SetupState::PlanReady {
            return Err(String::from("No current infrastructure plan is ready."));
        }
        self.state = SetupState::AwaitingInfrastructureConfirmation;
        Ok(())
    }

    pub fn confirm_infrastructure(&mut self) -> Result<(), String> {
        if self.state != SetupState::AwaitingInfrastructureConfirmation {
            return Err(String::from(
                "Billable infrastructure cannot be created before explicit confirmation.",
            ));
        }
        self.state = SetupState::Provisioning;
        Ok(())
    }

    pub fn infrastructure_ready(&mut self) {
        self.state = SetupState::InfrastructureReady;
    }

    pub fn request_test_generation(&mut self) -> Result<(), String> {
        if !matches!(
            self.state,
            SetupState::InfrastructureReady | SetupState::Ready
        ) {
            return Err(String::from(
                "Verify the managed FLUX.2 infrastructure before test generation.",
            ));
        }
        self.state = SetupState::AwaitingTestGenerationConfirmation;
        Ok(())
    }

    pub fn cancel_test_generation(&mut self) {
        if self.state == SetupState::AwaitingTestGenerationConfirmation {
            self.state = SetupState::InfrastructureReady;
        }
    }

    pub fn confirm_test_generation(&mut self) -> Result<(), String> {
        if self.state != SetupState::AwaitingTestGenerationConfirmation {
            return Err(String::from(
                "Test generation cannot start before explicit compute-charge confirmation.",
            ));
        }
        self.state = SetupState::TestGenerationRunning;
        Ok(())
    }

    pub fn track_test_job(&mut self, job_id: String) {
        self.test_job_id = Some(job_id);
        self.last_generation_state = Some(String::from("Submitting to RunPod"));
    }

    pub fn update_test_job(&mut self, job_id: &str, state: &str, terminal_success: bool) {
        if self.test_job_id.as_deref() != Some(job_id) {
            return;
        }
        self.last_generation_state = Some(state.to_string());
        if terminal_success {
            self.state = SetupState::Ready;
        } else if matches!(state, "failed" | "cancelled") {
            // Infrastructure remains usable after a generation failure, so a
            // human can inspect diagnostics and retry without reprovisioning.
            self.state = SetupState::InfrastructureReady;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticStatus {
    Pass,
    Warning,
    Fail,
}

impl DiagnosticStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warning => "WARNING",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightCheck {
    pub name: String,
    pub status: DiagnosticStatus,
    pub summary: String,
    pub detail: String,
    pub http_status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreflightReport {
    pub checks: Vec<PreflightCheck>,
}

impl PreflightReport {
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DiagnosticStatus::Fail)
    }
}

#[derive(Debug, Clone)]
pub struct PreflightOutcome {
    pub report: PreflightReport,
    pub discovery: Option<ProvisioningDiscovery>,
}

/// Run all setup checks without creating or modifying any RunPod resource.
pub fn run_preflight(paths: &PortalPaths, store: &NexusStore, api_key: &str) -> PreflightOutcome {
    let mut checks = vec![local_storage_check(paths, store)];
    let image = check_public_ghcr_image(FLUX_WORKER_IMAGE);
    let image_status = match image.status {
        PublicImageStatus::Available => DiagnosticStatus::Pass,
        PublicImageStatus::RegistryUnavailable => DiagnosticStatus::Fail,
        PublicImageStatus::Unauthorized
        | PublicImageStatus::TagMissing
        | PublicImageStatus::InvalidResponse => DiagnosticStatus::Fail,
    };
    checks.push(PreflightCheck {
        name: String::from("GHCR worker image"),
        status: image_status,
        summary: if image.status == PublicImageStatus::Available {
            format!("{FLUX_WORKER_IMAGE} is public")
        } else {
            format!("{FLUX_WORKER_IMAGE} is unavailable")
        },
        detail: image.detail,
        http_status: image.http_status,
    });

    let client = match RunPodInfrastructureClient::new(api_key.to_string()) {
        Ok(client) => client,
        Err(error) => {
            checks.push(fail("RunPod API", "API key is missing", error));
            return PreflightOutcome {
                report: PreflightReport { checks },
                discovery: None,
            };
        }
    };
    // Each permission is exercised with a separate documented read. That
    // yields an actionable diagnostic instead of treating every failure as a
    // vague invalid-key error.
    let endpoints = client.list_endpoints();
    let volumes = client.list_network_volumes();
    let gpus = client.list_gpus();
    let data_centers = client.list_data_centers();
    let billing = client.recent_billing();

    let authenticated = [
        &endpoints.as_ref().map(|_| ()),
        &volumes.as_ref().map(|_| ()),
    ]
    .into_iter()
    .any(Result::is_ok);
    if authenticated {
        checks.push(pass(
            "RunPod API",
            "API key authenticated",
            "RunPod accepted an authenticated infrastructure read.",
        ));
    } else {
        let detail = endpoints
            .as_ref()
            .err()
            .or_else(|| volumes.as_ref().err())
            .cloned()
            .unwrap_or_else(|| String::from("RunPod authentication could not be verified."));
        checks.push(fail(
            "RunPod API",
            "Authentication could not be verified",
            detail,
        ));
    }

    let mut permission_errors = Vec::new();
    for (name, result) in [
        ("Serverless endpoints", endpoints.as_ref().map(|_| ())),
        ("Network Volumes", volumes.as_ref().map(|_| ())),
        ("GPU catalog", gpus.as_ref().map(|_| ())),
        ("data center catalog", data_centers.as_ref().map(|_| ())),
    ] {
        if let Err(error) = result {
            permission_errors.push(format!("{name}: {error}"));
        }
    }
    if permission_errors.is_empty() {
        checks.push(pass(
            "RunPod permissions",
            "Required read permissions are available",
            "Endpoint, volume, GPU, and data center reads all succeeded.",
        ));
    } else {
        checks.push(fail(
            "RunPod permissions",
            "One or more required reads failed",
            permission_errors.join(" "),
        ));
    }

    match &volumes {
        Ok(items) => checks.push(pass(
            "Existing Network Volume",
            &format!("Discovered {} Network Volume(s)", items.len()),
            "Discovery is read-only; no volume was created.",
        )),
        Err(error) => checks.push(fail(
            "Existing Network Volume",
            "Could not list Network Volumes",
            error.clone(),
        )),
    }
    match &endpoints {
        Ok(items) => checks.push(pass(
            "Existing image endpoint",
            &format!("Discovered {} Serverless endpoint(s)", items.len()),
            "Discovery is read-only; no endpoint was created or changed.",
        )),
        Err(error) => checks.push(fail(
            "Existing image endpoint",
            "Could not list Serverless endpoints",
            error.clone(),
        )),
    }

    match (&gpus, &data_centers) {
        (Ok(gpus), Ok(data_centers)) => {
            let compatible = gpus.iter().any(|gpu| {
                gpu.memory >= 48
                    && gpu.availability.as_deref() != Some("NONE")
                    && gpu.data_centers.iter().any(|gpu_dc| {
                        gpu_dc.availability != "NONE"
                            && data_centers.iter().any(|data_center| {
                                data_center.id == gpu_dc.id
                                    && data_center
                                        .network_volume_types
                                        .iter()
                                        .any(|kind| kind == "STANDARD")
                            })
                    })
            });
            checks.push(if compatible {
                pass(
                    "GPU availability",
                    "Compatible 48+ GB GPU discovered",
                    "At least one currently listed GPU shares a data center with STANDARD Network Volume support.",
                )
            } else {
                fail(
                    "GPU availability",
                    "No compatible 48+ GB GPU is currently available",
                    "Retry later or inspect RunPod's Serverless GPU availability and Network Volume regions.",
                )
            });
        }
        _ => checks.push(fail(
            "GPU availability",
            "GPU availability could not be evaluated",
            "The GPU or data center catalog read failed.",
        )),
    }

    let recent_billing = match billing {
        Ok(summary) => {
            checks.push(pass(
                "Billing visibility",
                "Recent billing data is readable",
                "Billing visibility is informational and does not authorize resource creation.",
            ));
            summary
        }
        Err(error) => {
            checks.push(PreflightCheck {
                name: String::from("Billing visibility"),
                status: DiagnosticStatus::Warning,
                summary: String::from("Billing history is unavailable"),
                detail: format!(
                    "Provisioning can continue when all required infrastructure reads pass. {error}"
                ),
                http_status: status_code_from_error(&error),
            });
            BillingSummary { total_usd: None }
        }
    };

    let report = PreflightReport { checks };
    let discovery = if !report.has_failures() {
        match (endpoints, volumes, gpus, data_centers) {
            (Ok(endpoints), Ok(volumes), Ok(gpus), Ok(data_centers)) => {
                Some(ProvisioningDiscovery {
                    endpoints,
                    volumes,
                    gpus,
                    data_centers,
                    recent_billing,
                })
            }
            _ => None,
        }
    } else {
        None
    };
    PreflightOutcome { report, discovery }
}

pub fn local_storage_check(paths: &PortalPaths, store: &NexusStore) -> PreflightCheck {
    let result = (|| {
        fs::create_dir_all(paths.nexus_dir()).map_err(|error| {
            format!(
                "Could not create Nexus root '{}': {error}",
                paths.nexus_dir().display()
            )
        })?;
        store.health_check()?;
        let images = paths.media_dir().join("images");
        if !images.is_dir() {
            return Err(format!(
                "Media image directory '{}' is missing.",
                images.display()
            ));
        }
        probe_directory(&images, "media/images")?;
        if !paths.temp_dir().is_dir() {
            return Err(format!(
                "Nexus temp path '{}' is not a directory.",
                paths.temp_dir().display()
            ));
        }
        probe_directory(&paths.temp_dir(), "temp")?;
        Ok::<(), String>(())
    })();

    match result {
        Ok(()) => pass(
            "Local Nexus storage",
            "Database, media, and temp storage are ready",
            &format!("Nexus root: {}", paths.nexus_dir().display()),
        ),
        Err(error) => fail(
            "Local Nexus storage",
            "Local storage is not writable",
            error,
        ),
    }
}

fn probe_directory(directory: &Path, label: &str) -> Result<(), String> {
    let probe = directory.join(format!(".portal-write-probe-{}", uuid::Uuid::new_v4()));
    let write_result = write_probe(&probe);
    let cleanup_result = if probe.exists() {
        fs::remove_file(&probe).map_err(|error| {
            format!(
                "Could not remove {label} write probe '{}': {error}",
                probe.display()
            )
        })
    } else {
        Ok(())
    };
    write_result?;
    cleanup_result
}

fn write_probe(path: &Path) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("Could not create temporary write probe: {error}"))?;
    file.write_all(b"portal-storage-probe")
        .map_err(|error| format!("Could not write temporary storage probe: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Could not flush temporary storage probe: {error}"))
}

fn pass(name: &str, summary: &str, detail: &str) -> PreflightCheck {
    PreflightCheck {
        name: name.to_string(),
        status: DiagnosticStatus::Pass,
        summary: summary.to_string(),
        detail: detail.to_string(),
        http_status: None,
    }
}

fn fail(name: &str, summary: &str, detail: impl Into<String>) -> PreflightCheck {
    let detail = detail.into();
    let http_status = status_code_from_error(&detail);
    PreflightCheck {
        name: name.to_string(),
        status: DiagnosticStatus::Fail,
        summary: summary.to_string(),
        detail,
        http_status,
    }
}

fn status_code_from_error(error: &str) -> Option<u16> {
    [401, 403, 404, 409, 422, 429]
        .into_iter()
        .find(|code| error.contains(&format!("HTTP {code}")))
}

pub struct DiagnosticContext<'a> {
    pub setup: &'a SetupSession,
    pub provisioning: &'a ProvisioningState,
    pub local_media_root: &'a Path,
    pub last_job_id: Option<&'a str>,
    pub last_job_status: Option<&'a str>,
    pub api_key: Option<&'a str>,
}

/// Build a deliberately allow-listed report. Prompts, conversations, keyring
/// contents, environment variables, and request headers are never inputs.
pub fn diagnostic_report(context: DiagnosticContext<'_>) -> String {
    let mut lines = vec![
        format!("Portal version: {}", env!("CARGO_PKG_VERSION")),
        format!("OS: {}", std::env::consts::OS),
        format!("Worker image: {FLUX_WORKER_IMAGE}"),
        format!("Setup state: {}", context.setup.state.label()),
        format!("Local media root: {}", context.local_media_root.display()),
        format!(
            "Network Volume: {}",
            context
                .provisioning
                .network_volume_name
                .as_deref()
                .unwrap_or("not configured")
        ),
        format!(
            "Volume ID: {}",
            context
                .provisioning
                .network_volume_id
                .as_deref()
                .unwrap_or("not configured")
        ),
        format!(
            "Endpoint ID: {}",
            context
                .provisioning
                .image_endpoint_id
                .as_deref()
                .unwrap_or("not configured")
        ),
        format!(
            "GPU: {}",
            context
                .provisioning
                .gpu_type_id
                .as_deref()
                .unwrap_or("not selected")
        ),
        format!(
            "Data center: {}",
            context
                .provisioning
                .data_center_id
                .as_deref()
                .unwrap_or("not selected")
        ),
        format!("Last job ID: {}", context.last_job_id.unwrap_or("none")),
        format!(
            "Last generation state: {}",
            context
                .setup
                .last_generation_state
                .as_deref()
                .or(context.last_job_status)
                .unwrap_or("none")
        ),
        String::from("Checks:"),
    ];
    for check in &context.setup.diagnostics {
        let http = check
            .http_status
            .map(|status| format!(" (HTTP {status})"))
            .unwrap_or_default();
        lines.push(format!(
            "- {} | {} | {}{} | {}",
            check.status.label(),
            check.name,
            check.summary,
            http,
            check.detail
        ));
    }
    sanitize_report(&lines.join("\n"), context.api_key)
}

fn sanitize_report(report: &str, api_key: Option<&str>) -> String {
    let mut sanitized = report.to_string();
    if let Some(secret) = api_key.filter(|value| !value.is_empty()) {
        sanitized = sanitized.replace(secret, "[REDACTED]");
    }
    sanitized
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.contains("authorization:")
                || lower.contains("hf_token")
                || lower.contains("keyring contents")
            {
                String::from("[REDACTED SENSITIVE LINE]")
            } else if let Some(index) = lower.find("bearer ") {
                format!("{}Bearer [REDACTED]", &line[..index])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_transitions_require_both_explicit_confirmations() {
        let mut setup = SetupSession::new(true);
        setup.begin_preflight().expect("preflight may start");
        setup.finish_preflight(PreflightReport::default(), false);
        assert_eq!(setup.state, SetupState::DiscoveryReady);
        assert!(setup.confirm_infrastructure().is_err());
        setup.plan_ready().expect("plan may be accepted");
        setup
            .await_infrastructure_confirmation()
            .expect("plan may await confirmation");
        setup
            .confirm_infrastructure()
            .expect("explicit infrastructure confirmation is valid");
        setup.infrastructure_ready();
        assert!(setup.confirm_test_generation().is_err());
        setup
            .request_test_generation()
            .expect("ready infrastructure allows a test request");
        setup
            .confirm_test_generation()
            .expect("explicit generation confirmation is valid");
        assert_eq!(setup.state, SetupState::TestGenerationRunning);
    }

    #[test]
    fn preflight_pass_warning_fail_are_distinct() {
        let report = PreflightReport {
            checks: vec![
                pass("one", "ok", "ok"),
                PreflightCheck {
                    name: String::from("two"),
                    status: DiagnosticStatus::Warning,
                    summary: String::from("limited"),
                    detail: String::from("billing only"),
                    http_status: Some(403),
                },
            ],
        };
        assert!(!report.has_failures());
        let failed = PreflightReport {
            checks: vec![fail("three", "bad", String::from("failed"))],
        };
        assert!(failed.has_failures());
    }

    #[test]
    fn local_storage_probe_removes_its_temporary_file() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let paths = PortalPaths::new(directory.path());
        paths.initialize().expect("paths should initialize");
        let store = NexusStore::open(paths.database_path()).expect("database should open");
        let check = local_storage_check(&paths, &store);
        assert_eq!(check.status, DiagnosticStatus::Pass);
        let entries = fs::read_dir(paths.temp_dir())
            .expect("temp directory should be readable")
            .collect::<Result<Vec<_>, _>>()
            .expect("temp entries should load");
        assert!(entries.is_empty());
    }

    #[test]
    fn local_storage_probe_reports_an_unusable_temp_path() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let paths = PortalPaths::new(directory.path());
        paths.initialize().expect("paths should initialize");
        let store = NexusStore::open(paths.database_path()).expect("database should open");
        fs::remove_dir(paths.temp_dir()).expect("temp directory should be removable");
        fs::write(paths.temp_dir(), "not a directory").expect("blocking file should be created");
        let check = local_storage_check(&paths, &store);
        assert_eq!(check.status, DiagnosticStatus::Fail);
        assert!(check.summary.contains("not writable"));
    }

    #[test]
    fn diagnostic_report_redacts_secrets_and_excludes_prompts() {
        let mut setup = SetupSession::new(true);
        setup.diagnostics.push(PreflightCheck {
            name: String::from("RunPod API"),
            status: DiagnosticStatus::Fail,
            summary: String::from("provider error"),
            detail: String::from(
                "Authorization: Bearer super-secret; HF_TOKEN=also-secret; super-secret",
            ),
            http_status: Some(401),
        });
        let report = diagnostic_report(DiagnosticContext {
            setup: &setup,
            provisioning: &ProvisioningState::default(),
            local_media_root: Path::new("/safe/media"),
            last_job_id: Some("job-1"),
            last_job_status: Some("failed"),
            api_key: Some("super-secret"),
        });
        assert!(!report.contains("super-secret"));
        assert!(!report.contains("also-secret"));
        assert!(!report.contains("Authorization:"));
        assert!(!report.contains("generated prompt"));
        assert!(report.contains("Portal version"));
        assert!(report.contains("job-1"));
    }
}
