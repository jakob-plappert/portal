#![allow(
    dead_code,
    reason = "compiler request/result contracts precede provider integration"
)]

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::generation::GenerationMode;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PromptCompilerMode {
    #[default]
    Manual,
    LocalHttp,
    RunPod,
}

impl fmt::Display for PromptCompilerMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Manual => "manual",
            Self::LocalHttp => "local_http",
            Self::RunPod => "runpod",
        })
    }
}

impl FromStr for PromptCompilerMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "manual" => Ok(Self::Manual),
            "local_http" => Ok(Self::LocalHttp),
            "runpod" => Ok(Self::RunPod),
            _ => Err(format!("Unknown prompt compiler mode '{value}'.")),
        }
    }
}

/// The compiler receives domain context, never UI widgets or provider-specific
/// request types. A future local or RunPod LLM can therefore consume the same
/// serializable request without becoming Portal's source of truth.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromptCompileRequest {
    pub user_idea: String,
    pub generation_mode: GenerationMode,
    pub model_id: String,
    pub character_ids: Vec<String>,
    pub reference_media_ids: Vec<String>,
    pub project_context: Option<String>,
    pub conversation_context: Vec<PromptContextMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromptContextMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromptCompileResult {
    pub final_prompt: String,
    pub negative_prompt: Option<String>,
    /// This is a concise user-facing summary, never private chain-of-thought.
    pub explanation: String,
    pub generation_mode: GenerationMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_types_round_trip_through_json() {
        let request = PromptCompileRequest {
            user_idea: String::from("Sarah walks through rainy Tokyo."),
            generation_mode: GenerationMode::TextToVideo,
            model_id: String::from("ltx-2.5"),
            character_ids: vec![String::from("character-1")],
            reference_media_ids: vec![String::from("media-1")],
            project_context: Some(String::from("Night sequence")),
            conversation_context: vec![PromptContextMessage {
                role: String::from("user"),
                content: String::from("Make it cinematic."),
            }],
        };

        let json = serde_json::to_string(&request).expect("request should serialize");
        let decoded: PromptCompileRequest =
            serde_json::from_str(&json).expect("request should deserialize");
        assert_eq!(decoded, request);

        let result = PromptCompileResult {
            final_prompt: String::from("A cinematic tracking shot in rainy Tokyo."),
            negative_prompt: None,
            explanation: String::from("Expanded lighting and camera movement for LTX-2.5."),
            generation_mode: GenerationMode::TextToVideo,
        };
        let json = serde_json::to_string(&result).expect("result should serialize");
        let decoded: PromptCompileResult =
            serde_json::from_str(&json).expect("result should deserialize");
        assert_eq!(decoded, result);
    }
}
