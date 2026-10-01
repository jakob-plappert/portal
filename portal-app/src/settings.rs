#![allow(
    dead_code,
    reason = "settings diagnostics include paths used by tests and future UI"
)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::prompt::PromptCompilerMode;

const KEYRING_SERVICE: &str = "portal-nexus";
const KEYRING_USER: &str = "runpod-api-key";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunPodSettings {
    pub prompt_endpoint_id: String,
    pub image_endpoint_id: String,
    pub video_endpoint_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptCompilerSettings {
    pub mode: PromptCompilerMode,
    pub local_http_url: String,
}

impl Default for PromptCompilerSettings {
    fn default() -> Self {
        Self {
            mode: PromptCompilerMode::Manual,
            local_http_url: String::from("http://127.0.0.1:8080/v1/chat/completions"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PortalSettings {
    pub runpod: RunPodSettings,
    pub prompt_compiler: PromptCompilerSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeySource {
    Keyring,
    Environment,
    Missing,
}

// Deliberately no `Debug`: derived output would reveal the in-memory secret.
#[derive(Clone)]
pub struct ApiKeyState {
    /// The secret is kept in Rust memory only long enough to authorize calls.
    /// It is never copied into a Slint property or non-secret settings file.
    pub value: Option<String>,
    pub source: ApiKeySource,
}

#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<PortalSettings, String> {
        if !self.path.exists() {
            return Ok(PortalSettings::default());
        }
        let content = fs::read_to_string(&self.path)
            .map_err(|error| format!("Could not read settings: {error}"))?;
        toml::from_str(&content).map_err(|error| format!("Could not parse settings: {error}"))
    }

    pub fn save(&self, settings: &PortalSettings) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| String::from("Settings path has no parent directory."))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create settings directory: {error}"))?;
        let content = toml::to_string_pretty(settings)
            .map_err(|error| format!("Could not serialize settings: {error}"))?;
        fs::write(&self.path, content).map_err(|error| format!("Could not write settings: {error}"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn load_runpod_api_key() -> ApiKeyState {
    // The OS keyring is preferred. Access can legitimately fail on headless
    // developer machines, so the environment variable remains a non-persistent
    // fallback instead of turning application startup into an error.
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        && let Ok(value) = entry.get_password()
        && !value.trim().is_empty()
    {
        return ApiKeyState {
            value: Some(value),
            source: ApiKeySource::Keyring,
        };
    }

    if let Ok(value) = std::env::var("RUNPOD_API_KEY")
        && !value.trim().is_empty()
    {
        return ApiKeyState {
            value: Some(value),
            source: ApiKeySource::Environment,
        };
    }

    ApiKeyState {
        value: None,
        source: ApiKeySource::Missing,
    }
}

pub fn save_runpod_api_key(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(String::from("RunPod API key must not be empty."));
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|error| format!("Could not access the operating system keyring: {error}"))?;
    entry
        .set_password(value)
        .map_err(|error| format!("Could not save RunPod API key in the keyring: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_is_not_written_to_plain_settings() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let store = SettingsStore::new(directory.path().join("settings.toml"));
        let settings = PortalSettings {
            runpod: RunPodSettings {
                prompt_endpoint_id: String::from("prompt-endpoint"),
                image_endpoint_id: String::from("image-endpoint"),
                video_endpoint_id: String::from("video-endpoint"),
            },
            prompt_compiler: PromptCompilerSettings::default(),
        };
        store.save(&settings).expect("settings should save");
        let content = fs::read_to_string(store.path()).expect("settings should be readable");
        assert!(!content.contains("RUNPOD_SECRET_VALUE"));
        assert!(!content.contains("api_key"));
        assert_eq!(store.load().expect("settings should load"), settings);
    }
}
