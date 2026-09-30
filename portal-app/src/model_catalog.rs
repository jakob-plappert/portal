#![allow(
    dead_code,
    reason = "future compiler and policy UI consume full catalog metadata"
)]

use crate::generation::{GenerationMode, MediaKind};

pub const FLUX_2_DEV_ID: &str = "flux-2-dev";
pub const LTX_2_5_ID: &str = "ltx-2.5";
pub const WAN_2_2_ID: &str = "wan-2.2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCapability {
    None,
    GeneratedSynchronized,
    ExternalInput,
    GeneratedOrExternal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdultContentPolicy {
    NotSupported,
    ProviderDependent,
    PermittedWhenLawful,
}

/// These are capability facts used for validation, not a moderation engine.
/// The two prohibited fields remain explicit invariants for every profile.
#[derive(Debug, Clone, Copy)]
pub struct ContentPolicy {
    pub adult_content: AdultContentPolicy,
    pub sexual_content_involving_minors: bool,
    pub nonconsensual_explicit_real_people: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PromptGuidance {
    pub use_camera_language: bool,
    pub use_motion_language: bool,
    pub dialogue_is_meaningful: bool,
    pub emphasize_subject_continuity: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ModelProfile {
    pub id: &'static str,
    pub display_name: &'static str,
    pub media_kind: MediaKind,
    pub supported_modes: &'static [GenerationMode],
    pub supports_reference_images: bool,
    pub supports_video_input: bool,
    pub audio: AudioCapability,
    pub prompt_compilation_recommended: bool,
    pub notes: &'static str,
    pub policy: ContentPolicy,
    pub prompt_guidance: PromptGuidance,
}

impl ModelProfile {
    pub fn supports_mode(&self, mode: GenerationMode) -> bool {
        self.supported_modes.contains(&mode)
    }
}

const SAFE_PROVIDER_POLICY: ContentPolicy = ContentPolicy {
    adult_content: AdultContentPolicy::ProviderDependent,
    sexual_content_involving_minors: false,
    nonconsensual_explicit_real_people: false,
};

const IMAGE_MODES: &[GenerationMode] = &[GenerationMode::TextToImage, GenerationMode::ImageToImage];

const VIDEO_MODES: &[GenerationMode] = &[
    GenerationMode::TextToVideo,
    GenerationMode::ImageToVideo,
    GenerationMode::VideoToVideo,
];

pub const MODELS: &[ModelProfile] = &[
    ModelProfile {
        id: FLUX_2_DEV_ID,
        display_name: "FLUX.2 [dev]",
        media_kind: MediaKind::Image,
        supported_modes: IMAGE_MODES,
        supports_reference_images: true,
        supports_video_input: false,
        audio: AudioCapability::None,
        prompt_compilation_recommended: true,
        notes: "High-quality still-image generation and image-guided editing.",
        policy: SAFE_PROVIDER_POLICY,
        prompt_guidance: PromptGuidance {
            use_camera_language: true,
            use_motion_language: false,
            dialogue_is_meaningful: false,
            emphasize_subject_continuity: true,
        },
    },
    ModelProfile {
        id: LTX_2_5_ID,
        display_name: "LTX-2.5",
        media_kind: MediaKind::Video,
        supported_modes: VIDEO_MODES,
        supports_reference_images: true,
        supports_video_input: true,
        audio: AudioCapability::GeneratedSynchronized,
        prompt_compilation_recommended: true,
        notes: "Video generation with useful camera, motion, continuity, and audio guidance.",
        policy: SAFE_PROVIDER_POLICY,
        prompt_guidance: PromptGuidance {
            use_camera_language: true,
            use_motion_language: true,
            dialogue_is_meaningful: true,
            emphasize_subject_continuity: true,
        },
    },
    ModelProfile {
        id: WAN_2_2_ID,
        display_name: "Wan 2.2",
        media_kind: MediaKind::Video,
        supported_modes: VIDEO_MODES,
        supports_reference_images: true,
        supports_video_input: true,
        audio: AudioCapability::ExternalInput,
        prompt_compilation_recommended: true,
        notes: "Video generation supporting text, image, and video conditioning.",
        policy: SAFE_PROVIDER_POLICY,
        prompt_guidance: PromptGuidance {
            use_camera_language: true,
            use_motion_language: true,
            dialogue_is_meaningful: false,
            emphasize_subject_continuity: true,
        },
    },
];

pub fn find_model(id: &str) -> Option<&'static ModelProfile> {
    MODELS.iter().find(|profile| profile.id == id)
}

pub fn validate_model_mode(
    model_id: &str,
    mode: GenerationMode,
) -> Result<&'static ModelProfile, String> {
    // `Option` expresses that catalog lookup may find no profile. Converting
    // it to `Result` attaches the actionable error required by the UI.
    let profile = find_model(model_id)
        .ok_or_else(|| format!("Model '{model_id}' is not in the Portal catalog."))?;

    if !profile.supports_mode(mode) {
        return Err(format!(
            "{} does not support {}.",
            profile.display_name,
            mode.display_name()
        ));
    }

    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_required_profiles() {
        assert!(find_model(FLUX_2_DEV_ID).is_some());
        assert!(find_model(LTX_2_5_ID).is_some());
        assert!(find_model(WAN_2_2_ID).is_some());
    }

    #[test]
    fn capability_checks_are_model_specific() {
        assert!(validate_model_mode(FLUX_2_DEV_ID, GenerationMode::TextToImage).is_ok());
        assert!(validate_model_mode(LTX_2_5_ID, GenerationMode::ImageToVideo).is_ok());
        assert!(validate_model_mode(WAN_2_2_ID, GenerationMode::VideoToVideo).is_ok());
        assert!(validate_model_mode(FLUX_2_DEV_ID, GenerationMode::TextToVideo).is_err());
        assert!(validate_model_mode(LTX_2_5_ID, GenerationMode::ImageToImage).is_err());
    }

    #[test]
    fn prohibited_content_is_never_enabled() {
        for model in MODELS {
            assert!(!model.policy.sexual_content_involving_minors);
            assert!(!model.policy.nonconsensual_explicit_real_people);
        }
    }
}
