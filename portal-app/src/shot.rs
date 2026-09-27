use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::project::Project;

// Serde derives both directions because a Shot is the complete TOML schema:
// creation serializes it, while opening a project deserializes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shot {
    pub number: u32,
    pub title: String,
    pub brief: String,
}

impl Shot {
    pub fn create(project: &Project, title: &str, brief: &str) -> Result<Self, String> {
        // All arguments are borrowed, so creating a shot does not take the
        // Project or callback strings away from their owners. The returned
        // Shot allocates owned strings for data that must survive this call.
        let title = title.trim();
        let brief = brief.trim();

        if title.is_empty() {
            return Err(String::from("Shot title must not be empty."));
        }

        let shots_dir = project.path.join("shots");

        fs::create_dir_all(&shots_dir)
            .map_err(|error| format!("Could not create shots directory: {error}"))?;

        // A failure to inspect existing shots must stop creation; `?`
        // propagates that `Err(String)` and unwraps the number on success.
        let number = next_shot_number(&shots_dir)?;

        let shot = Self {
            number,
            title: title.to_string(),
            brief: brief.to_string(),
        };

        // The derived `Serialize` implementation lets the TOML crate encode
        // the borrowed Shot without consuming it, so it can still be returned.
        let serialized = toml::to_string_pretty(&shot)
            .map_err(|error| format!("Could not serialize shot: {error}"))?;

        let filename = format!("shot_{number:04}.toml");

        let path = shots_dir.join(filename);

        fs::write(&path, serialized).map_err(|error| format!("Could not save shot: {error}"))?;

        Ok(shot)
    }

    pub fn load_all(project: &Project) -> Result<Vec<Self>, String> {
        let shots_dir = project.path.join("shots");

        fs::create_dir_all(&shots_dir)
            .map_err(|error| format!("Could not create shots directory: {error}"))?;

        let entries = fs::read_dir(&shots_dir)
            .map_err(|error| format!("Could not read shots directory: {error}"))?;

        let mut shots = Vec::new();

        // Directory iteration and file parsing are fallible operations. Each
        // error is enriched with context and propagated rather than producing
        // a partially loaded project.
        for entry in entries {
            let entry = entry.map_err(|error| format!("Could not read shot entry: {error}"))?;

            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            // Both "has an extension" and "extension is valid UTF-8" are
            // optional, so the composed result is `Option<&str>`.
            let extension = path.extension().and_then(|extension| extension.to_str());

            if extension != Some("toml") {
                continue;
            }

            let content = fs::read_to_string(&path)
                .map_err(|error| format!("Could not read '{}': {error}", path.display()))?;

            // Serde's generated `Deserialize` implementation reconstructs an
            // owned Shot from the temporary TOML string.
            let shot = toml::from_str::<Shot>(&content)
                .map_err(|error| format!("Could not parse '{}': {error}", path.display()))?;

            shots.push(shot);
        }

        shots.sort_by_key(|shot| shot.number);

        Ok(shots)
    }
}

fn next_shot_number(shots_dir: &Path) -> Result<u32, String> {
    let entries = fs::read_dir(shots_dir)
        .map_err(|error| format!("Could not read shots directory: {error}"))?;

    let mut highest_number = 0;

    // Only filenames shaped like `shot_<u32>` participate. `let-else` keeps
    // each rejected case local and leaves the successful path unindented.
    for entry in entries {
        let entry = entry.map_err(|error| format!("Could not read shot entry: {error}"))?;

        let path = entry.path();

        let Some(stem) = path.file_stem() else {
            continue;
        };

        let Some(stem) = stem.to_str() else {
            continue;
        };

        let Some(number_text) = stem.strip_prefix("shot_") else {
            continue;
        };

        // This pattern match deliberately ignores parse errors from unrelated
        // files; unlike filesystem errors above, they do not prevent scanning.
        let Ok(number) = number_text.parse::<u32>() else {
            continue;
        };

        highest_number = highest_number.max(number);
    }

    Ok(highest_number + 1)
}
