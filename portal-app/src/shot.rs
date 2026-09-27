use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::project::Project;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shot {
    pub number: u32,
    pub title: String,
    pub brief: String,
}

impl Shot {
    pub fn create(project: &Project, title: &str, brief: &str) -> Result<Self, String> {
        let title = title.trim();
        let brief = brief.trim();

        if title.is_empty() {
            return Err(String::from("Shot title must not be empty."));
        }

        let shots_dir = project.path.join("shots");

        fs::create_dir_all(&shots_dir)
            .map_err(|error| format!("Could not create shots directory: {error}"))?;

        let number = next_shot_number(&shots_dir)?;

        let shot = Self {
            number,
            title: title.to_string(),
            brief: brief.to_string(),
        };

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

        for entry in entries {
            let entry = entry.map_err(|error| format!("Could not read shot entry: {error}"))?;

            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let extension = path.extension().and_then(|extension| extension.to_str());

            if extension != Some("toml") {
                continue;
            }

            let content = fs::read_to_string(&path)
                .map_err(|error| format!("Could not read '{}': {error}", path.display()))?;

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

        let Ok(number) = number_text.parse::<u32>() else {
            continue;
        };

        highest_number = highest_number.max(number);
    }

    Ok(highest_number + 1)
}
