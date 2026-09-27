use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

// `Project` is the application's runtime representation. It owns its strings
// and `PathBuf`, so it can safely outlive the temporary directory entries and
// TOML text from which it was constructed.
#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub folder_name: String,
    pub path: PathBuf,
}

// Serde's derive macros generate the serialization/deserialization code that
// maps this deliberately small disk schema to and from `project.toml`. Keeping
// it separate avoids persisting runtime-only details such as an absolute path.
#[derive(Debug, Serialize, Deserialize)]
struct ProjectFile {
    name: String,
    version: u32,
}

impl Project {
    pub fn create(name: &str) -> Result<Self, String> {
        // The caller lends us `&str`; `trim()` returns another borrowed slice
        // rather than allocating. We create owned strings only for data the
        // returned Project and its TOML file must retain.
        let name = name.trim();

        if name.is_empty() {
            return Err(String::from("Project name must not be empty."));
        }

        // `?` propagates the validation error immediately. On success it
        // unwraps the folder name from `Result<String, String>`.
        let folder_name = project_folder_name(name)?;

        let projects_dir = projects_dir();
        let project_dir = projects_dir.join(&folder_name);

        if project_dir.exists() {
            return Err(format!("Project '{name}' already exists."));
        }

        // Filesystem errors are converted into user-facing strings. The final
        // `?` returns that error to the caller instead of panicking.
        fs::create_dir_all(&project_dir)
            .map_err(|error| format!("Could not create project directory: {error}"))?;

        create_project_directories(&project_dir)?;

        let project_file = ProjectFile {
            name: name.to_string(),
            version: 1,
        };

        // Serde supplies ProjectFile's `Serialize` implementation; `toml`
        // uses it to produce the stable on-disk representation.
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

        // `read_dir` yields a `Result` for every entry because a directory can
        // change or become unreadable during iteration. Propagating an error
        // avoids silently presenting an incomplete project list.
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("Could not read project directory entry: {error}"))?;

            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect project entry: {error}"))?;

            // Non-project files and directories without project.toml are
            // ignored so portaldata may contain other runtime content.
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

        // The explicit type tells serde which schema to deserialize. Invalid
        // TOML remains a recoverable `Result::Err` with its path attached.
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
    // Borrowing `&Path` lets the caller keep ownership of its PathBuf. Each
    // `join` creates the owned child path needed by the filesystem operation.
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
    // `std::env::var` returns `Result` because a variable may be missing or not
    // valid Unicode. A valid override is useful for development and tests;
    // either error falls back to the repository-local runtime directory.
    if let Ok(path) = std::env::var("PORTAL_DATA_DIR") {
        return PathBuf::from(path);
    }

    // `env!` embeds Cargo's manifest directory at compile time. The parent is
    // an invariant of this repository layout, so a clear `expect` message is
    // preferable to carrying an impossible `Option` through the application.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("portal-app must have a parent directory")
        .join("portaldata")
}

fn validate_project_folder_name(folder_name: &str) -> Result<(), String> {
    // Accept exactly one normal path component. Pattern matching rejects
    // absolute paths, `..`, and nested paths before joining user-controlled
    // input beneath portaldata/projects.
    let mut components = Path::new(folder_name).components();

    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),

        _ => Err(String::from("Invalid project folder name.")),
    }
}

fn project_folder_name(name: &str) -> Result<String, String> {
    // Build a filesystem-friendly owned slug. The intermediate split slices
    // borrow from the collected String only for this expression; the final
    // `join` returns a new independent String.
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
