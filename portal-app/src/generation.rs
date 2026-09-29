use serde::{Deserialize, Serialize};

use crate::shot::Shot;

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
    use super::GenerationSpec;
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
}
