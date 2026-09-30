#![allow(
    dead_code,
    reason = "this module includes stable next-milestone domain contracts"
)]

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::shot::Shot;

/// The five user-facing ways Nexus can transform text and reference media.
/// An enum prevents invalid spellings from leaking into domain logic; strings
/// are used only at storage, JSON, and Slint boundaries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GenerationMode {
    TextToImage,
    ImageToImage,
    TextToVideo,
    ImageToVideo,
    VideoToVideo,
}

impl GenerationMode {
    pub const ALL: [Self; 5] = [
        Self::TextToImage,
        Self::ImageToImage,
        Self::TextToVideo,
        Self::ImageToVideo,
        Self::VideoToVideo,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Self::TextToImage => "Text to Image",
            Self::ImageToImage => "Image to Image",
            Self::TextToVideo => "Text to Video",
            Self::ImageToVideo => "Image to Video",
            Self::VideoToVideo => "Video to Video",
        }
    }

    pub fn output_kind(self) -> MediaKind {
        match self {
            Self::TextToImage | Self::ImageToImage => MediaKind::Image,
            Self::TextToVideo | Self::ImageToVideo | Self::VideoToVideo => MediaKind::Video,
        }
    }

    pub fn required_input(self) -> Option<MediaKind> {
        match self {
            Self::TextToImage | Self::TextToVideo => None,
            Self::ImageToImage | Self::ImageToVideo => Some(MediaKind::Image),
            Self::VideoToVideo => Some(MediaKind::Video),
        }
    }
}

impl fmt::Display for GenerationMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TextToImage => "text_to_image",
            Self::ImageToImage => "image_to_image",
            Self::TextToVideo => "text_to_video",
            Self::ImageToVideo => "image_to_video",
            Self::VideoToVideo => "video_to_video",
        })
    }
}

impl FromStr for GenerationMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "text_to_image" => Ok(Self::TextToImage),
            "image_to_image" => Ok(Self::ImageToImage),
            "text_to_video" => Ok(Self::TextToVideo),
            "image_to_video" => Ok(Self::ImageToVideo),
            "video_to_video" => Ok(Self::VideoToVideo),
            _ => Err(format!("Unknown generation mode '{value}'.")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
}

impl fmt::Display for MediaKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        })
    }
}

impl FromStr for MediaKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "image" => Ok(Self::Image),
            "video" => Ok(Self::Video),
            "audio" => Ok(Self::Audio),
            _ => Err(format!("Unknown media kind '{value}'.")),
        }
    }
}

// `GenerationSpec` describes what Portal wants to create. It deliberately has
// no ComfyUI, FLUX, sampler, or workflow fields: translating this stable domain
// request into settings for a particular backend belongs to later adapter code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationSpec {
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    pub seed: Option<u64>,
}

/// Generic generation settings stop at the boundary shared by all backends.
/// Model-specific samplers and ComfyUI node identifiers belong in a future
/// worker adapter, not in the user's durable intent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationIntent {
    pub mode: GenerationMode,
    pub model_id: String,
    pub user_idea: String,
    pub compiled_prompt: Option<String>,
    pub negative_prompt: Option<String>,
    pub seed: Option<u64>,
    pub parameters: GenerationParameters,
    pub character_ids: Vec<String>,
    pub reference_media_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationParameters {
    Image {
        width: u32,
        height: u32,
    },
    Video {
        width: u32,
        height: u32,
        duration_seconds: f32,
        audio_input_media_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NexusAction {
    PrepareGeneration {
        user_idea: String,
    },
    GenerateMedia {
        job_id: String,
    },
    SaveCharacter {
        name: String,
        media_asset_id: String,
    },
    UpdateCharacter {
        character_id: String,
    },
    SelectCharacter {
        character_id: String,
    },
}

impl GenerationSpec {
    pub fn from_shot(shot: &Shot) -> Self {
        // `&Shot` is a shared borrow. This function may read the shot without
        // taking ownership of it away from AppState, which remains Portal's
        // source of truth after the specification has been prepared.
        let brief = shot.brief.trim();

        let prompt = if brief.is_empty() {
            shot.title.clone()
        } else {
            brief.to_string()
        };

        Self {
            prompt,
            width: 1024,
            height: 1024,

            // `Option<u64>` represents a seed that may or may not be fixed.
            // `Some(value)` would request that exact seed; `None` means Portal
            // leaves seed selection automatic for this preview milestone.
            seed: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GenerationMode, GenerationSpec, MediaKind};
    use crate::shot::Shot;

    fn shot(title: &str, brief: &str) -> Shot {
        Shot {
            number: 1,
            title: title.to_string(),
            brief: brief.to_string(),
        }
    }

    #[test]
    fn non_empty_brief_becomes_prompt() {
        let spec = GenerationSpec::from_shot(&shot("A fallback title", "  A wide forest  "));

        assert_eq!(spec.prompt, "A wide forest");
    }

    #[test]
    fn empty_brief_falls_back_to_title() {
        let spec = GenerationSpec::from_shot(&shot("Laura enters the kitchen", ""));

        assert_eq!(spec.prompt, "Laura enters the kitchen");
    }

    #[test]
    fn whitespace_only_brief_falls_back_to_title() {
        let spec = GenerationSpec::from_shot(&shot("Close-up", "  \n\t  "));

        assert_eq!(spec.prompt, "Close-up");
    }

    #[test]
    fn default_dimensions_are_square_1024() {
        let spec = GenerationSpec::from_shot(&shot("Establishing shot", "A quiet street"));

        assert_eq!(spec.width, 1024);
        assert_eq!(spec.height, 1024);
    }

    #[test]
    fn default_seed_is_automatic() {
        let spec = GenerationSpec::from_shot(&shot("Establishing shot", "A quiet street"));

        assert_eq!(spec.seed, None);
    }

    #[test]
    fn modes_describe_required_inputs_and_outputs() {
        assert_eq!(GenerationMode::ALL.len(), 5);
        assert_eq!(GenerationMode::TextToImage.required_input(), None);
        assert_eq!(
            GenerationMode::ImageToVideo.required_input(),
            Some(MediaKind::Image)
        );
        assert_eq!(
            GenerationMode::VideoToVideo.required_input(),
            Some(MediaKind::Video)
        );
        assert_eq!(GenerationMode::TextToVideo.output_kind(), MediaKind::Video);
    }
}
