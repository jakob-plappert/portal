use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub folder_name: String,
    pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProjectFile {
    name: String,
    version: u32,
}

impl Project {
    pub fn create(name: &str) -> Result<Self, String> {
        let name = name.trim();

        if name.is_empty() {
            return Err(String::from("Project name must not be empty."));
        }

        let folder_name = project_folder_name(name)?;

        let projects_dir = projects_dir();
        let project_dir = projects_dir.join(&folder_name);

        if project_dir.exists() {
            return Err(format!("Project '{name}' already exists."));
        }

        fs::create_dir_all(&project_dir)
            .map_err(|error| format!("Could not create project directory: {error}"))?;

        create_project_directories(&project_dir)?;

        let project_file = ProjectFile {
            name: name.to_string(),
            version: 1,
        };

        let content = toml::to_string_pretty(&project_file)
            .map_err(|error| format!("Could not serialize project.toml: {error}"))?;

        let project_file_path = project_dir.join("project.toml");

        fs::write(&project_file_path, content)
            .map_err(|error| format!("Could not write project.toml: {error}"))?;

        Ok(Self {
            name: name.to_string(),
            folder_name,
            path: project_dir,
        })
    }

    pub fn open(folder_name: &str) -> Result<Self, String> {
        validate_project_folder_name(folder_name)?;

        let project_dir = projects_dir().join(folder_name);

        if !project_dir.is_dir() {
            return Err(format!("Project folder '{folder_name}' does not exist."));
        }

        Self::load_from_directory(folder_name.to_string(), project_dir)
    }

    pub fn list() -> Result<Vec<Self>, String> {
        let projects_dir = projects_dir();

        fs::create_dir_all(&projects_dir)
            .map_err(|error| format!("Could not create projects directory: {error}"))?;

        let entries = fs::read_dir(&projects_dir)
            .map_err(|error| format!("Could not read projects directory: {error}"))?;

        let mut projects = Vec::new();

        for entry in entries {
            let entry = entry
                .map_err(|error| format!("Could not read project directory entry: {error}"))?;

            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect project entry: {error}"))?;

            if !file_type.is_dir() {
                continue;
            }

            let project_dir = entry.path();

            let project_file = project_dir.join("project.toml");

            if !project_file.is_file() {
                continue;
            }

            let folder_name = entry.file_name().to_string_lossy().to_string();

            let project = Self::load_from_directory(folder_name, project_dir)?;

            projects.push(project);
        }

        projects.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));

        Ok(projects)
    }

    fn load_from_directory(folder_name: String, project_dir: PathBuf) -> Result<Self, String> {
        let project_file_path = project_dir.join("project.toml");

        let content = fs::read_to_string(&project_file_path).map_err(|error| {
            format!("Could not read '{}': {error}", project_file_path.display())
        })?;

        let project_file = toml::from_str::<ProjectFile>(&content).map_err(|error| {
            format!("Could not parse '{}': {error}", project_file_path.display())
        })?;

        Ok(Self {
            name: project_file.name,
            folder_name,
            path: project_dir,
        })
    }
}

fn create_project_directories(project_dir: &Path) -> Result<(), String> {
    for directory in [
        "characters",
        "locations",
        "references",
        "shots",
        "generations",
    ] {
        fs::create_dir_all(project_dir.join(directory))
            .map_err(|error| format!("Could not create '{directory}' directory: {error}"))?;
    }

    Ok(())
}

fn projects_dir() -> PathBuf {
    portal_data_dir().join("projects")
}

fn portal_data_dir() -> PathBuf {
    if let Ok(path) = std::env::var("PORTAL_DATA_DIR") {
        return PathBuf::from(path);
    }

    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("portal-app must have a parent directory")
        .join("portaldata")
}

fn validate_project_folder_name(folder_name: &str) -> Result<(), String> {
    let mut components = Path::new(folder_name).components();

    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),

        _ => Err(String::from("Invalid project folder name.")),
    }
}

fn project_folder_name(name: &str) -> Result<String, String> {
    let folder_name = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if folder_name.is_empty() {
        return Err(String::from(
            "Project name does not contain usable characters.",
        ));
    }

    Ok(folder_name)
}
