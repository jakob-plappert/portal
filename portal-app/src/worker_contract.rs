#![allow(
    dead_code,
    reason = "response types await the first deployed media worker"
)]

use serde::{Deserialize, Serialize};

use crate::generation::{GenerationIntent, GenerationMode, GenerationParameters};

pub const MEDIA_WORKER_REQUEST_VERSION: u32 = 1;

/// This is Portal's stable boundary with a future media worker. The worker is
/// responsible for translating it into FLUX/LTX/Wan and ComfyUI details.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaWorkerRequest {
    pub request_version: u32,
    pub generation_mode: GenerationMode,
    pub model_id: String,
    pub compiled_prompt: String,
    pub negative_prompt: Option<String>,
    pub seed: Option<u64>,
    pub dimensions: Option<Dimensions>,
    pub duration_seconds: Option<f32>,
    pub references: Vec<RemoteReference>,
}

impl MediaWorkerRequest {
    pub fn from_intent(intent: &GenerationIntent) -> Result<Self, String> {
        // The intent remains backend-independent. This conversion only shapes
        // Portal's stable public worker contract; a worker will later turn it
        // into model-specific ComfyUI nodes and parameters.
        let compiled_prompt = intent
            .compiled_prompt
            .clone()
            .ok_or_else(|| String::from("A compiled or manually entered prompt is required."))?;
        let (dimensions, duration_seconds) = match &intent.parameters {
            GenerationParameters::Image { width, height } => (
                Some(Dimensions {
                    width: *width,
                    height: *height,
                }),
                None,
            ),
            GenerationParameters::Video {
                width,
                height,
                duration_seconds,
                ..
            } => (
                Some(Dimensions {
                    width: *width,
                    height: *height,
                }),
                Some(*duration_seconds),
            ),
        };

        Ok(Self {
            request_version: MEDIA_WORKER_REQUEST_VERSION,
            generation_mode: intent.mode,
            model_id: intent.model_id.clone(),
            compiled_prompt,
            negative_prompt: intent.negative_prompt.clone(),
            seed: intent.seed,
            dimensions,
            duration_seconds,
            // Local media IDs cannot be sent to a remote worker directly. A
            // future upload adapter resolves them to short-lived URLs first.
            references: Vec::new(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteReference {
    pub media_asset_id: String,
    pub kind: String,
    /// Workers need an accessible URL. Portal will later upload only the
    /// selected local reference and provide its temporary location here.
    pub temporary_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaWorkerResponse {
    pub status: WorkerResultStatus,
    pub artifacts: Vec<OutputArtifact>,
    pub metadata: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerResultStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutputArtifact {
    pub kind: String,
    pub mime_type: Option<String>,
    pub filename: Option<String>,
    pub content: ArtifactContent,
}

/// Inline data is suitable only for small results. Large videos are returned
/// by a temporary URL so they are not inflated into a giant JSON/base64 body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum ArtifactContent {
    DownloadUrl { url: String },
    InlineBase64 { data: String },
}
