#![allow(
    dead_code,
    reason = "poll and cancel transport precede deployed endpoint UI actions"
)]

use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::worker_contract::MediaWorkerRequest;
use crate::{
    media_library::MediaLibrary,
    paths::PortalPaths,
    storage::{JobStatus, NexusStore},
    worker_contract::{MediaWorkerResponse, WorkerResultStatus},
};

pub const RUNPOD_QUEUE_BASE_URL: &str = "https://api.runpod.ai/v2";

// Deliberately no `Debug`: derived debug output would expose the API key.
#[derive(Clone)]
pub struct RunPodQueueClient {
    api_key: String,
    base_url: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPodJobState {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(rename = "executionTime", default)]
    pub execution_time_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueRequestMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueRequest {
    method: QueueRequestMethod,
    url: String,
    action: &'static str,
}

impl RunPodQueueClient {
    pub fn new(api_key: String) -> Result<Self, String> {
        if api_key.trim().is_empty() {
            return Err(String::from("RunPod API key is not configured."));
        }
        Ok(Self {
            api_key,
            base_url: String::from(RUNPOD_QUEUE_BASE_URL),
            agent: ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_global(Some(Duration::from_secs(60)))
                .build()
                .into(),
        })
    }

    pub fn submit_job(
        &self,
        endpoint_id: &str,
        request: &MediaWorkerRequest,
    ) -> Result<RunPodJobState, String> {
        self.validate_endpoint(endpoint_id)?;
        let url = format!("{}/{}/run", self.base_url, endpoint_id);
        // The generic transport only wraps Portal's stable worker contract in
        // RunPod's `input` envelope. It knows nothing about ComfyUI nodes or a
        // particular model's workflow payload.
        let body = serde_json::json!({ "input": request });
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(body)
            .map_err(queue_error)?;
        response
            .body_mut()
            .read_json::<RunPodJobState>()
            .map_err(|error| format!("RunPod returned an invalid submission response: {error}"))
    }

    pub fn poll_job(&self, endpoint_id: &str, job_id: &str) -> Result<RunPodJobState, String> {
        let request = self.status_request(endpoint_id, job_id)?;
        self.execute_queue_request(request)
    }

    pub fn cancel_job(&self, endpoint_id: &str, job_id: &str) -> Result<RunPodJobState, String> {
        let request = self.cancel_request(endpoint_id, job_id)?;
        self.execute_queue_request(request)
    }

    fn status_request(&self, endpoint_id: &str, job_id: &str) -> Result<QueueRequest, String> {
        self.queue_request(endpoint_id, job_id, "status", QueueRequestMethod::Get)
    }

    fn cancel_request(&self, endpoint_id: &str, job_id: &str) -> Result<QueueRequest, String> {
        self.queue_request(endpoint_id, job_id, "cancel", QueueRequestMethod::Post)
    }

    fn queue_request(
        &self,
        endpoint_id: &str,
        job_id: &str,
        action: &'static str,
        method: QueueRequestMethod,
    ) -> Result<QueueRequest, String> {
        self.validate_endpoint(endpoint_id)?;
        if job_id.trim().is_empty() {
            return Err(String::from("RunPod job ID must not be empty."));
        }
        Ok(QueueRequest {
            method,
            url: format!("{}/{}/{}/{}", self.base_url, endpoint_id, action, job_id),
            action,
        })
    }

    fn execute_queue_request(&self, request: QueueRequest) -> Result<RunPodJobState, String> {
        // Status is a read and uses GET. Cancellation changes remote state and
        // must use POST. Keeping the method in the tested request description
        // prevents these similarly shaped URLs from silently sharing GET.
        let response = match request.method {
            QueueRequestMethod::Get => self
                .agent
                .get(&request.url)
                .header("Authorization", &format!("Bearer {}", self.api_key))
                .call(),
            QueueRequestMethod::Post => self
                .agent
                .post(&request.url)
                .header("Authorization", &format!("Bearer {}", self.api_key))
                .send_empty(),
        };
        let mut response = response.map_err(queue_error)?;
        response
            .body_mut()
            .read_json::<RunPodJobState>()
            .map_err(|error| {
                format!(
                    "RunPod returned an invalid {} response: {error}",
                    request.action
                )
            })
    }

    fn validate_endpoint(&self, endpoint_id: &str) -> Result<(), String> {
        if endpoint_id.trim().is_empty() {
            return Err(String::from("RunPod endpoint is not configured."));
        }
        if !endpoint_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        }) {
            return Err(String::from(
                "RunPod endpoint ID contains invalid characters.",
            ));
        }
        Ok(())
    }
}

fn queue_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(401) => String::from("RunPod rejected the API key (HTTP 401)."),
        ureq::Error::StatusCode(403) => {
            String::from("RunPod denied access to this endpoint (HTTP 403).")
        }
        ureq::Error::StatusCode(404) => {
            String::from("The RunPod endpoint or job no longer exists (HTTP 404).")
        }
        ureq::Error::StatusCode(429) => {
            String::from("RunPod rate-limited queue polling. Wait briefly and retry.")
        }
        ureq::Error::Timeout(_) => String::from("The RunPod queue request timed out."),
        other => format!("RunPod queue request failed: {other}"),
    }
}

// Deliberately no `Debug`: this command carries the API key in memory.
#[derive(Clone)]
pub enum RunPodCommand {
    Submit {
        local_job_id: String,
        api_key: String,
        endpoint_id: String,
        request: MediaWorkerRequest,
    },
    Reconcile {
        local_job_id: String,
        api_key: String,
        endpoint_id: String,
        remote_job_id: String,
        model_id: String,
    },
}

#[derive(Debug, Clone)]
pub enum RunPodEvent {
    Updated {
        local_job_id: String,
        status: JobStatus,
        message: String,
    },
    Failed {
        local_job_id: String,
        error: String,
    },
}

#[derive(Debug, Clone)]
pub struct RunPodWorker {
    sender: Sender<RunPodCommand>,
}

impl RunPodWorker {
    pub fn start(
        store: NexusStore,
        paths: PortalPaths,
        on_event: impl Fn(RunPodEvent) + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel::<RunPodCommand>();

        // `thread::spawn` requires captured values to be `Send + 'static`:
        // ownership moves into the new thread and may outlive this function.
        // Commands cross the channel as owned values, so the UI thread never
        // lends a reference that could become invalid during network work.
        thread::spawn(move || {
            while let Ok(command) = receiver.recv() {
                match command {
                    RunPodCommand::Submit {
                        local_job_id,
                        api_key,
                        endpoint_id,
                        request,
                    } => {
                        let model_id = request.model_id.clone();
                        let result = RunPodQueueClient::new(api_key).and_then(|client| {
                            let remote = client.submit_job(&endpoint_id, &request)?;
                            // Persist the remote ID before polling. If Portal
                            // exits now, startup reconciliation still knows
                            // which provider job belongs to this local row.
                            store.update_job(
                                &local_job_id,
                                JobStatus::Queued,
                                Some(&remote.id),
                                None,
                                None,
                            )?;
                            on_event(RunPodEvent::Updated {
                                local_job_id: local_job_id.clone(),
                                status: JobStatus::Queued,
                                message: format!("Queued on RunPod ({}).", remote.status),
                            });
                            poll_until_terminal(
                                &client,
                                &store,
                                &paths,
                                &endpoint_id,
                                &local_job_id,
                                &remote.id,
                                &model_id,
                                &on_event,
                            )
                        });
                        match result {
                            Ok(()) => {}
                            Err(error) => on_event(RunPodEvent::Failed {
                                local_job_id: local_job_id.clone(),
                                error,
                            }),
                        }
                    }
                    RunPodCommand::Reconcile {
                        local_job_id,
                        api_key,
                        endpoint_id,
                        remote_job_id,
                        model_id,
                    } => {
                        let result = RunPodQueueClient::new(api_key).and_then(|client| {
                            poll_until_terminal(
                                &client,
                                &store,
                                &paths,
                                &endpoint_id,
                                &local_job_id,
                                &remote_job_id,
                                &model_id,
                                &on_event,
                            )
                        });
                        if let Err(error) = result {
                            on_event(RunPodEvent::Failed {
                                local_job_id,
                                error,
                            });
                        }
                    }
                }
            }
        });

        Self { sender }
    }

    pub fn send(&self, command: RunPodCommand) -> Result<(), String> {
        self.sender
            .send(command)
            .map_err(|_| String::from("RunPod background worker has stopped."))
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "explicit queue, storage, identity, and callback boundaries are clearer than a generic polling context"
)]
fn poll_until_terminal(
    client: &RunPodQueueClient,
    store: &NexusStore,
    paths: &PortalPaths,
    endpoint_id: &str,
    local_job_id: &str,
    remote_job_id: &str,
    model_id: &str,
    on_event: &impl Fn(RunPodEvent),
) -> Result<(), String> {
    let mut consecutive_errors = 0;
    loop {
        match client.poll_job(endpoint_id, remote_job_id) {
            Ok(remote) => {
                consecutive_errors = 0;
                match classify_status(&remote.status) {
                    QueueStatus::Queued => update_progress(
                        store,
                        on_event,
                        local_job_id,
                        JobStatus::Queued,
                        "Waiting in the RunPod queue.",
                    )?,
                    QueueStatus::Running => update_progress(
                        store,
                        on_event,
                        local_job_id,
                        JobStatus::Running,
                        "FLUX.2 worker is running.",
                    )?,
                    QueueStatus::Completed => {
                        if let Some(execution_time) = remote.execution_time_ms {
                            store.update_job_metrics(
                                local_job_id,
                                None,
                                None,
                                Some(execution_time),
                            )?;
                        }
                        return ingest_completed_output(
                            store,
                            paths,
                            local_job_id,
                            model_id,
                            remote.output,
                            on_event,
                        );
                    }
                    QueueStatus::Failed => {
                        let error = remote.error.unwrap_or_else(|| {
                            format!("RunPod job ended with status {}.", remote.status)
                        });
                        store.update_job(
                            local_job_id,
                            JobStatus::Failed,
                            None,
                            None,
                            Some(&error),
                        )?;
                        return Err(error);
                    }
                    QueueStatus::Cancelled => {
                        store.update_job(local_job_id, JobStatus::Cancelled, None, None, None)?;
                        on_event(RunPodEvent::Updated {
                            local_job_id: local_job_id.to_string(),
                            status: JobStatus::Cancelled,
                            message: String::from("RunPod job was cancelled."),
                        });
                        return Ok(());
                    }
                    QueueStatus::Unknown => {
                        return Err(format!(
                            "RunPod returned unknown job status '{}'.",
                            remote.status
                        ));
                    }
                }
            }
            Err(error) => {
                consecutive_errors += 1;
                if consecutive_errors >= 3 {
                    return Err(format!(
                        "Could not poll RunPod job after three attempts: {error}"
                    ));
                }
            }
        }
        // Three seconds provides useful UI progress without hammering the
        // queue status endpoint. Sleeping happens only on this worker thread.
        thread::sleep(Duration::from_secs(3));
    }
}

fn update_progress(
    store: &NexusStore,
    on_event: &impl Fn(RunPodEvent),
    local_job_id: &str,
    status: JobStatus,
    message: &str,
) -> Result<(), String> {
    store.update_job(local_job_id, status, None, None, None)?;
    on_event(RunPodEvent::Updated {
        local_job_id: local_job_id.to_string(),
        status,
        message: message.to_string(),
    });
    Ok(())
}

fn ingest_completed_output(
    store: &NexusStore,
    paths: &PortalPaths,
    local_job_id: &str,
    model_id: &str,
    output: Option<serde_json::Value>,
    on_event: &impl Fn(RunPodEvent),
) -> Result<(), String> {
    let output = output.ok_or_else(|| String::from("Completed RunPod job returned no output."))?;
    let response: MediaWorkerResponse = serde_json::from_value(output)
        .map_err(|error| format!("Worker returned an invalid media response: {error}"))?;
    if response.status != WorkerResultStatus::Completed {
        return Err(response.error.unwrap_or_else(|| {
            format!("Worker response was {:?}, not completed.", response.status)
        }));
    }
    if response.artifacts.is_empty() {
        return Err(String::from("Worker completed without a media artifact."));
    }

    update_progress(
        store,
        on_event,
        local_job_id,
        JobStatus::Downloading,
        "Downloading generated media to Nexus local storage.",
    )?;
    let library = MediaLibrary::new(paths.clone(), store.clone());
    let mut first_media_id = None;
    for artifact in &response.artifacts {
        let asset = library.receive_remote_artifact(artifact, local_job_id, model_id)?;
        first_media_id.get_or_insert(asset.id);
    }
    let output_media_id = first_media_id
        .as_deref()
        .ok_or_else(|| String::from("No media was ingested from the completed job."))?;
    // Completed is deliberately the final write: a download, disk, or SQLite
    // failure above leaves the job Downloading/Failed, never falsely complete.
    store.update_job(
        local_job_id,
        JobStatus::Completed,
        None,
        Some(output_media_id),
        None,
    )?;
    on_event(RunPodEvent::Updated {
        local_job_id: local_job_id.to_string(),
        status: JobStatus::Completed,
        message: String::from("Generation completed and was saved to the local Media Library."),
    });
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

fn classify_status(value: &str) -> QueueStatus {
    match value {
        "IN_QUEUE" => QueueStatus::Queued,
        "IN_PROGRESS" => QueueStatus::Running,
        "COMPLETED" => QueueStatus::Completed,
        "FAILED" | "TIMED_OUT" => QueueStatus::Failed,
        "CANCELLED" => QueueStatus::Cancelled,
        _ => QueueStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_endpoint_is_rejected_before_network_access() {
        let client = RunPodQueueClient::new(String::from("not-a-real-key"))
            .expect("non-empty key should construct client");
        let error = client
            .poll_job("bad/endpoint", "job")
            .expect_err("unsafe endpoint should be rejected");
        assert!(error.contains("invalid characters"));
    }

    #[test]
    fn status_request_uses_get_and_status_path() {
        let client = RunPodQueueClient::new(String::from("not-a-real-key"))
            .expect("non-empty key should construct client");
        let request = client
            .status_request("image-endpoint", "job-123")
            .expect("request should be constructed without network access");

        assert_eq!(request.method, QueueRequestMethod::Get);
        assert_eq!(
            request.url,
            "https://api.runpod.ai/v2/image-endpoint/status/job-123"
        );
    }

    #[test]
    fn cancel_request_uses_post_and_cancel_path() {
        let client = RunPodQueueClient::new(String::from("not-a-real-key"))
            .expect("non-empty key should construct client");
        let request = client
            .cancel_request("video-endpoint", "job-456")
            .expect("request should be constructed without network access");

        assert_eq!(request.method, QueueRequestMethod::Post);
        assert_eq!(
            request.url,
            "https://api.runpod.ai/v2/video-endpoint/cancel/job-456"
        );
    }

    #[test]
    fn terminal_queue_states_are_explicit() {
        assert_eq!(classify_status("COMPLETED"), QueueStatus::Completed);
        assert_eq!(classify_status("FAILED"), QueueStatus::Failed);
        assert_eq!(classify_status("TIMED_OUT"), QueueStatus::Failed);
        assert_eq!(classify_status("CANCELLED"), QueueStatus::Cancelled);
        assert_eq!(classify_status("NEW_PROVIDER_STATE"), QueueStatus::Unknown);
    }

    #[test]
    fn successful_worker_output_is_local_before_job_completes() {
        use crate::generation::GenerationMode;
        use crate::worker_contract::{ArtifactContent, OutputArtifact};
        use base64::Engine;

        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let paths = PortalPaths::new(directory.path());
        paths.initialize().expect("paths should initialize");
        let store = NexusStore::open(paths.database_path()).expect("store should open");
        let job = store
            .create_job(
                GenerationMode::TextToImage,
                "flux-2-dev",
                "test",
                Some("test"),
                Some(1),
            )
            .expect("job should save");
        let response = MediaWorkerResponse {
            status: WorkerResultStatus::Completed,
            artifacts: vec![OutputArtifact {
                kind: String::from("image"),
                mime_type: Some(String::from("image/png")),
                filename: Some(String::from("remote/unsafe.png")),
                content: ArtifactContent::InlineBase64 {
                    data: base64::engine::general_purpose::STANDARD
                        .encode(b"\x89PNG\r\n\x1a\nPNG bytes"),
                },
            }],
            metadata: serde_json::json!({}),
            error: None,
        };
        let events = std::sync::Mutex::new(Vec::new());
        ingest_completed_output(
            &store,
            &paths,
            &job.id,
            "flux-2-dev",
            Some(serde_json::to_value(response).expect("response should serialize")),
            &|event| events.lock().expect("events lock").push(event),
        )
        .expect("output should ingest");
        let loaded = store
            .list_jobs()
            .expect("jobs should load")
            .into_iter()
            .find(|item| item.id == job.id)
            .expect("job should remain");
        assert_eq!(loaded.status, JobStatus::Completed);
        let asset = store
            .media_asset(loaded.output_media_id.as_deref().expect("output ID"))
            .expect("asset query should work")
            .expect("asset should exist");
        assert!(
            paths
                .absolute_media_path(&asset.relative_path)
                .expect("safe path")
                .is_file()
        );
    }

    #[test]
    fn failed_media_ingestion_never_marks_job_completed() {
        use crate::generation::GenerationMode;
        use crate::worker_contract::{ArtifactContent, OutputArtifact};

        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let paths = PortalPaths::new(directory.path());
        paths.initialize().expect("paths should initialize");
        let store = NexusStore::open(paths.database_path()).expect("store should open");
        let job = store
            .create_job(
                GenerationMode::TextToImage,
                "flux-2-dev",
                "test",
                Some("test"),
                None,
            )
            .expect("job should save");
        let response = MediaWorkerResponse {
            status: WorkerResultStatus::Completed,
            artifacts: vec![OutputArtifact {
                kind: String::from("image"),
                mime_type: Some(String::from("image/png")),
                filename: Some(String::from("result.png")),
                content: ArtifactContent::InlineBase64 {
                    data: String::from("not valid base64"),
                },
            }],
            metadata: serde_json::json!({}),
            error: None,
        };
        assert!(
            ingest_completed_output(
                &store,
                &paths,
                &job.id,
                "flux-2-dev",
                Some(serde_json::to_value(response).expect("response should serialize")),
                &|_| {},
            )
            .is_err()
        );
        let loaded = store
            .list_jobs()
            .expect("jobs should load")
            .into_iter()
            .find(|item| item.id == job.id)
            .expect("job should remain");
        assert_ne!(loaded.status, JobStatus::Completed);
        assert!(loaded.output_media_id.is_none());
    }
}
