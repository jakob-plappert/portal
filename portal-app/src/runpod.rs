#![allow(
    dead_code,
    reason = "poll and cancel transport precede deployed endpoint UI actions"
)]

use std::sync::mpsc::{self, Sender};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::worker_contract::MediaWorkerRequest;

const RUNPOD_SERVERLESS_BASE_URL: &str = "https://api.runpod.ai/v2";

// Deliberately no `Debug`: derived debug output would expose the API key.
#[derive(Clone)]
pub struct RunPodClient {
    api_key: String,
    base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPodJobState {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
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

impl RunPodClient {
    pub fn new(api_key: String) -> Result<Self, String> {
        if api_key.trim().is_empty() {
            return Err(String::from("RunPod API key is not configured."));
        }
        Ok(Self {
            api_key,
            base_url: String::from(RUNPOD_SERVERLESS_BASE_URL),
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
        let mut response = ureq::post(&url)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(body)
            .map_err(|error| format!("RunPod job submission failed: {error}"))?;
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
            QueueRequestMethod::Get => ureq::get(&request.url)
                .header("Authorization", &format!("Bearer {}", self.api_key))
                .call(),
            QueueRequestMethod::Post => ureq::post(&request.url)
                .header("Authorization", &format!("Bearer {}", self.api_key))
                .send_empty(),
        };
        let mut response = response
            .map_err(|error| format!("RunPod {} request failed: {error}", request.action))?;
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

// Deliberately no `Debug`: this command carries the API key in memory.
#[derive(Clone)]
pub enum RunPodCommand {
    Submit {
        local_job_id: String,
        api_key: String,
        endpoint_id: String,
        request: MediaWorkerRequest,
    },
}

#[derive(Debug, Clone)]
pub enum RunPodEvent {
    Submitted {
        local_job_id: String,
        remote_job_id: String,
        remote_status: String,
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
    pub fn start(on_event: impl Fn(RunPodEvent) + Send + 'static) -> Self {
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
                        let result = RunPodClient::new(api_key)
                            .and_then(|client| client.submit_job(&endpoint_id, &request));
                        match result {
                            Ok(remote) => on_event(RunPodEvent::Submitted {
                                local_job_id,
                                remote_job_id: remote.id,
                                remote_status: remote.status,
                            }),
                            Err(error) => on_event(RunPodEvent::Failed {
                                local_job_id,
                                error,
                            }),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_endpoint_is_rejected_before_network_access() {
        let client = RunPodClient::new(String::from("not-a-real-key"))
            .expect("non-empty key should construct client");
        let error = client
            .poll_job("bad/endpoint", "job")
            .expect_err("unsafe endpoint should be rejected");
        assert!(error.contains("invalid characters"));
    }

    #[test]
    fn status_request_uses_get_and_status_path() {
        let client = RunPodClient::new(String::from("not-a-real-key"))
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
        let client = RunPodClient::new(String::from("not-a-real-key"))
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
}
