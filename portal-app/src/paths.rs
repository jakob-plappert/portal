use std::fs;
use std::path::{Path, PathBuf};

/// All Nexus paths are derived from one root so metadata can store portable,
/// relative paths instead of machine-specific absolute paths.
#[derive(Debug, Clone)]
pub struct PortalPaths {
    root: PathBuf,
}

impl PortalPaths {
    pub fn discover() -> Self {
        if let Ok(path) = std::env::var("PORTAL_DATA_DIR") {
            return Self::new(path);
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("portal-app must have a parent directory")
            .join("portaldata");

        Self::new(root)
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn nexus_dir(&self) -> PathBuf {
        self.root.join("nexus")
    }

    pub fn database_path(&self) -> PathBuf {
        self.nexus_dir().join("nexus.sqlite3")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.nexus_dir().join("settings.toml")
    }

    pub fn media_dir(&self) -> PathBuf {
        self.nexus_dir().join("media")
    }

    pub fn imports_dir(&self) -> PathBuf {
        self.nexus_dir().join("imports")
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.nexus_dir().join("temp")
    }

    pub fn initialize(&self) -> Result<(), String> {
        // Media bytes deliberately live in ordinary directories. SQLite only
        // stores metadata, which keeps large images and videos streamable and
        // easy to inspect or back up with normal filesystem tools.
        for directory in [
            self.media_dir().join("images"),
            self.media_dir().join("videos"),
            self.media_dir().join("audio"),
            self.imports_dir(),
            self.temp_dir(),
        ] {
            fs::create_dir_all(&directory)
                .map_err(|error| format!("Could not create '{}': {error}", directory.display()))?;
        }

        Ok(())
    }

    pub fn absolute_media_path(&self, relative_path: &Path) -> Result<PathBuf, String> {
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(String::from(
                "Media path must be relative to portaldata/nexus.",
            ));
        }

        Ok(self.nexus_dir().join(relative_path))
    }
}
