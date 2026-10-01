#![allow(
    dead_code,
    reason = "remote receipt is ready before a media worker is deployed"
)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use base64::Engine;
use uuid::Uuid;

use crate::generation::MediaKind;
use crate::paths::PortalPaths;
use crate::storage::{MediaAsset, MediaSource, NewMediaAsset, NexusStore};
use crate::worker_contract::{ArtifactContent, OutputArtifact};

const MAX_INLINE_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct MediaLibrary {
    paths: PortalPaths,
    store: NexusStore,
}

impl MediaLibrary {
    pub fn new(paths: PortalPaths, store: NexusStore) -> Self {
        Self { paths, store }
    }

    pub fn import_file(&self, source: &Path, kind: MediaKind) -> Result<MediaAsset, String> {
        if !source.is_file() {
            return Err(format!(
                "'{}' is not a readable media file.",
                source.display()
            ));
        }
        let extension = safe_extension(source.file_name().and_then(|name| name.to_str()), None);
        let id = Uuid::new_v4().to_string();
        let relative_path = media_relative_path(kind, &id, &extension);
        let destination = self.paths.absolute_media_path(&relative_path)?;
        fs::copy(source, &destination).map_err(|error| {
            format!(
                "Could not import '{}' to '{}': {error}",
                source.display(),
                destination.display()
            )
        })?;

        let result = self.store.insert_media_asset(&NewMediaAsset {
            id,
            kind,
            relative_path,
            source: MediaSource::Imported,
            generation_job_id: None,
            model_id: None,
            width: None,
            height: None,
            duration_seconds: None,
            mime_type: None,
        });
        if result.is_err() {
            let _ = fs::remove_file(destination);
        }
        result
    }

    pub fn receive_remote_artifact(
        &self,
        artifact: &OutputArtifact,
        generation_job_id: &str,
        model_id: &str,
    ) -> Result<MediaAsset, String> {
        let kind = MediaKind::from_str(&artifact.kind)?;
        let extension = safe_extension(artifact.filename.as_deref(), artifact.mime_type.as_deref());
        let id = Uuid::new_v4().to_string();
        let relative_path = media_relative_path(kind, &id, &extension);
        let destination = self.paths.absolute_media_path(&relative_path)?;

        let write_result = match &artifact.content {
            ArtifactContent::DownloadUrl { url } => download_to_file(url, &destination),
            ArtifactContent::InlineBase64 { data } => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|error| format!("Could not decode inline artifact: {error}"))?;
                if decoded.len() > MAX_INLINE_BYTES {
                    return Err(String::from(
                        "Inline artifact is too large; the worker must provide a temporary URL.",
                    ));
                }
                fs::write(&destination, decoded).map_err(|error| {
                    format!(
                        "Could not save remote artifact '{}': {error}",
                        destination.display()
                    )
                })
            }
        };
        if let Err(error) = write_result {
            // A failed streamed download may leave a partial file. It has no
            // valid metadata and must not appear as usable local media.
            let _ = fs::remove_file(&destination);
            return Err(error);
        }

        // Metadata is inserted only after the complete file is local. If the
        // insert fails, cleanup avoids an untracked output in the media tree.
        let result = self.store.insert_media_asset(&NewMediaAsset {
            id,
            kind,
            relative_path,
            source: MediaSource::RemoteResult,
            generation_job_id: Some(generation_job_id.to_string()),
            model_id: Some(model_id.to_string()),
            width: None,
            height: None,
            duration_seconds: None,
            mime_type: artifact.mime_type.clone(),
        });
        if result.is_err() {
            let _ = fs::remove_file(destination);
        }
        result
    }
}

fn download_to_file(url: &str, destination: &Path) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(String::from("Remote artifact URL must use HTTP or HTTPS."));
    }
    let mut response = ureq::get(url)
        .call()
        .map_err(|error| format!("Could not download remote artifact: {error}"))?;
    let mut file = fs::File::create(destination)
        .map_err(|error| format!("Could not create '{}': {error}", destination.display()))?;
    io::copy(&mut response.body_mut().as_reader(), &mut file)
        .map_err(|error| format!("Could not write '{}': {error}", destination.display()))?;
    Ok(())
}

fn media_relative_path(kind: MediaKind, id: &str, extension: &str) -> PathBuf {
    let directory = match kind {
        MediaKind::Image => "images",
        MediaKind::Video => "videos",
        MediaKind::Audio => "audio",
    };
    PathBuf::from("media")
        .join(directory)
        .join(format!("{id}.{extension}"))
}

fn safe_extension(filename: Option<&str>, mime_type: Option<&str>) -> String {
    if let Some(extension) = filename
        .and_then(|name| Path::new(name).extension())
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 8
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
    {
        return extension.to_ascii_lowercase();
    }
    match mime_type {
        Some("image/jpeg") => String::from("jpg"),
        Some("image/webp") => String::from("webp"),
        Some("video/mp4") => String::from("mp4"),
        Some("audio/wav") => String::from("wav"),
        Some("audio/mpeg") => String::from("mp3"),
        _ => String::from("bin"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_artifact_becomes_local_file_and_metadata() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let paths = PortalPaths::new(directory.path());
        paths.initialize().expect("directories should initialize");
        let store = NexusStore::open(paths.database_path()).expect("database should open");
        let library = MediaLibrary::new(paths.clone(), store.clone());
        let artifact = OutputArtifact {
            kind: String::from("image"),
            mime_type: Some(String::from("image/png")),
            filename: Some(String::from("output.png")),
            content: ArtifactContent::InlineBase64 {
                data: base64::engine::general_purpose::STANDARD.encode(b"small image bytes"),
            },
        };
        let asset = library
            .receive_remote_artifact(&artifact, "job-1", "flux-2-dev")
            .expect("artifact should be received");
        assert!(!asset.relative_path.is_absolute());
        assert!(
            paths
                .absolute_media_path(&asset.relative_path)
                .expect("path should be safe")
                .is_file()
        );
        assert_eq!(
            store
                .media_asset(&asset.id)
                .expect("lookup should work")
                .expect("asset should exist")
                .generation_job_id
                .as_deref(),
            Some("job-1")
        );
    }
}
