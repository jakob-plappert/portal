use std::fs;
use std::path::{Component, Path, PathBuf};

const NEXUS_APP_ID: &str = "nexus";

/// `PortalPaths` derives every application's private storage area from one
/// root. Future apps use `app_dir("spybotics")`; only Nexus-specific filenames
/// need dedicated convenience helpers.
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

    pub fn apps_dir(&self) -> PathBuf {
        self.root.join("apps")
    }

    pub fn shared_dir(&self) -> PathBuf {
        self.root.join("shared")
    }

    pub fn app_dir(&self, app_id: &str) -> Result<PathBuf, String> {
        validate_app_id(app_id)?;
        Ok(self.apps_dir().join(app_id))
    }

    pub fn nexus_dir(&self) -> PathBuf {
        // This constant is controlled by Portal rather than external input.
        // Going through `app_dir` keeps Nexus on the same validated mechanism
        // that future internal apps will use.
        self.app_dir(NEXUS_APP_ID)
            .expect("the built-in Nexus app ID must remain valid")
    }

    pub fn database_path(&self) -> PathBuf {
        self.nexus_dir().join("nexus.sqlite3")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.nexus_dir().join("settings.toml")
    }

    pub fn infrastructure_path(&self) -> PathBuf {
        self.nexus_dir().join("infrastructure.toml")
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
        fs::create_dir_all(self.apps_dir()).map_err(|error| {
            format!(
                "Could not create Portal apps directory '{}': {error}",
                self.apps_dir().display()
            )
        })?;
        fs::create_dir_all(self.shared_dir()).map_err(|error| {
            format!(
                "Could not create Portal shared directory '{}': {error}",
                self.shared_dir().display()
            )
        })?;

        self.migrate_legacy_nexus_dir()?;

        // Media bytes deliberately live in ordinary app-scoped directories.
        // SQLite only stores metadata, which keeps large images and videos
        // streamable and easy to inspect or back up with filesystem tools.
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
        if relative_path.as_os_str().is_empty()
            || relative_path.is_absolute()
            || relative_path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(String::from(
                "Media path must be relative to portaldata/apps/nexus.",
            ));
        }

        Ok(self.nexus_dir().join(relative_path))
    }

    fn migrate_legacy_nexus_dir(&self) -> Result<(), String> {
        let legacy_dir = self.root.join(NEXUS_APP_ID);
        let nexus_dir = self.nexus_dir();

        // If both paths exist, the new app-scoped area wins. Leaving the old
        // directory untouched is conservative: Portal never merges or
        // overwrites two potentially different user data sets automatically.
        if !legacy_dir.exists() || nexus_dir.exists() {
            return Ok(());
        }
        let legacy_type = fs::symlink_metadata(&legacy_dir)
            .map_err(|error| format!("Could not inspect '{}': {error}", legacy_dir.display()))?
            .file_type();
        if !legacy_type.is_dir() {
            return Err(format!(
                "Cannot migrate legacy Nexus storage because '{}' is not a directory.",
                legacy_dir.display()
            ));
        }

        fs::rename(&legacy_dir, &nexus_dir).map_err(|error| {
            format!(
                "Could not safely migrate Nexus storage from '{}' to '{}': {error}",
                legacy_dir.display(),
                nexus_dir.display()
            )
        })
    }
}

pub(crate) fn validate_app_id(app_id: &str) -> Result<(), String> {
    // A deliberately small identifier alphabet produces readable directories
    // and rejects separators, absolute paths, `.` and `..` before `join` can
    // interpret them as filesystem navigation.
    if app_id.is_empty()
        || !app_id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '-'
                || character == '_'
        })
    {
        return Err(format!(
            "Invalid Portal app ID '{app_id}'; use lowercase letters, digits, '-' or '_'."
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_storage_is_scoped_and_shared_storage_is_separate() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let paths = PortalPaths::new(directory.path());
        paths.initialize().expect("storage should initialize");

        assert_eq!(
            paths.app_dir("spybotics").expect("app ID should be valid"),
            directory.path().join("apps/spybotics")
        );
        assert_eq!(paths.nexus_dir(), directory.path().join("apps/nexus"));
        assert_eq!(paths.shared_dir(), directory.path().join("shared"));
        assert_eq!(
            paths.database_path(),
            directory.path().join("apps/nexus/nexus.sqlite3")
        );
        assert_eq!(
            paths.settings_path(),
            directory.path().join("apps/nexus/settings.toml")
        );
        assert!(directory.path().join("apps/nexus/media/images").is_dir());
        assert!(directory.path().join("apps/nexus/media/videos").is_dir());
        assert!(directory.path().join("apps/nexus/media/audio").is_dir());
        assert!(directory.path().join("apps/nexus/imports").is_dir());
        assert!(directory.path().join("apps/nexus/temp").is_dir());
    }

    #[test]
    fn app_ids_cannot_escape_the_apps_directory() {
        let paths = PortalPaths::new("portaldata");
        for invalid in ["", ".", "..", "../nexus", "nexus/data", "/nexus", "Nexus"] {
            assert!(
                paths.app_dir(invalid).is_err(),
                "'{invalid}' should be rejected"
            );
        }
    }

    #[test]
    fn legacy_nexus_directory_is_moved_once() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let legacy_dir = directory.path().join("nexus");
        fs::create_dir_all(&legacy_dir).expect("legacy directory should be created");
        fs::write(legacy_dir.join("settings.toml"), "legacy=true")
            .expect("legacy marker should be written");

        let paths = PortalPaths::new(directory.path());
        paths.initialize().expect("migration should succeed");

        assert!(!legacy_dir.exists());
        assert_eq!(
            fs::read_to_string(paths.settings_path()).expect("migrated file should exist"),
            "legacy=true"
        );
    }

    #[test]
    fn existing_new_directory_is_never_overwritten() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let legacy_dir = directory.path().join("nexus");
        let new_dir = directory.path().join("apps/nexus");
        fs::create_dir_all(&legacy_dir).expect("legacy directory should be created");
        fs::create_dir_all(&new_dir).expect("new directory should be created");
        fs::write(legacy_dir.join("marker"), "legacy").expect("legacy marker should be written");
        fs::write(new_dir.join("marker"), "new").expect("new marker should be written");

        PortalPaths::new(directory.path())
            .initialize()
            .expect("existing new storage should remain usable");

        assert_eq!(
            fs::read_to_string(new_dir.join("marker")).expect("new marker should remain"),
            "new"
        );
        assert!(legacy_dir.join("marker").is_file());
    }

    #[test]
    fn unsafe_legacy_path_returns_a_clear_error() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        fs::write(directory.path().join("nexus"), "not a directory")
            .expect("legacy file should be written");

        let error = PortalPaths::new(directory.path())
            .initialize()
            .expect_err("a legacy file cannot be migrated as a directory");
        assert!(error.contains("not a directory"));
    }
}
