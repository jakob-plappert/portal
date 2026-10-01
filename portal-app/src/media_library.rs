#![allow(
    dead_code,
    reason = "remote receipt is ready before a media worker is deployed"
)]

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use uuid::Uuid;

use crate::generation::MediaKind;
use crate::paths::PortalPaths;
use crate::storage::{MediaAsset, MediaSource, NewMediaAsset, NexusStore};
use crate::worker_contract::{ArtifactContent, OutputArtifact};

const MAX_INLINE_BYTES: usize = 25 * 1024 * 1024;
const MAX_REMOTE_BYTES: u64 = 512 * 1024 * 1024;

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
        validate_media_type(kind, artifact.mime_type.as_deref())?;
        let extension = safe_extension(artifact.filename.as_deref(), artifact.mime_type.as_deref());
        let id = Uuid::new_v4().to_string();
        let relative_path = media_relative_path(kind, &id, &extension);
        let destination = self.paths.absolute_media_path(&relative_path)?;
        let temporary = destination.with_extension(format!("{extension}.part"));

        let write_result = match &artifact.content {
            ArtifactContent::DownloadUrl { url } => download_to_file(url, &temporary),
            ArtifactContent::InlineBase64 { data } => {
                let maximum_encoded_len = MAX_INLINE_BYTES.saturating_mul(4) / 3 + 4;
                if data.len() > maximum_encoded_len {
                    return Err(String::from(
                        "Inline artifact is too large; the worker must provide a temporary URL.",
                    ));
                }
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|error| format!("Could not decode inline artifact: {error}"))?;
                if decoded.len() > MAX_INLINE_BYTES {
                    return Err(String::from(
                        "Inline artifact is too large; the worker must provide a temporary URL.",
                    ));
                }
                fs::write(&temporary, decoded).map_err(|error| {
                    format!(
                        "Could not save remote artifact '{}': {error}",
                        temporary.display()
                    )
                })
            }
        };
        if let Err(error) = write_result {
            // A failed streamed download may leave a partial file. It has no
            // valid metadata and must not appear as usable local media.
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = validate_file_header(&temporary, kind, artifact.mime_type.as_deref()) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "Could not finalize remote artifact '{}': {error}",
                destination.display()
            ));
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
    if !url.starts_with("https://") {
        return Err(String::from("Remote artifact URL must use HTTPS."));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| format!("Could not download remote artifact: {error}"))?;
    let mut file = fs::File::create(destination)
        .map_err(|error| format!("Could not create '{}': {error}", destination.display()))?;
    // Read at most one byte past the limit so a malicious or misconfigured
    // remote URL cannot fill the user's disk indefinitely.
    let mut limited = response
        .body_mut()
        .as_reader()
        .take(MAX_REMOTE_BYTES.saturating_add(1));
    let written = io::copy(&mut limited, &mut file)
        .map_err(|error| format!("Could not write '{}': {error}", destination.display()))?;
    if written > MAX_REMOTE_BYTES {
        return Err(String::from(
            "Remote artifact exceeds the 512 MB ingestion limit.",
        ));
    }
    Ok(())
}

fn validate_media_type(kind: MediaKind, mime_type: Option<&str>) -> Result<(), String> {
    let supported = match kind {
        MediaKind::Image => ["image/png", "image/jpeg", "image/webp"].as_slice(),
        MediaKind::Video => ["video/mp4"].as_slice(),
        MediaKind::Audio => ["audio/wav", "audio/mpeg"].as_slice(),
    };
    let mime_type = mime_type.ok_or_else(|| String::from("Artifact MIME type is required."))?;
    if !supported.contains(&mime_type) {
        return Err(format!(
            "Artifact MIME type '{mime_type}' is not accepted for {kind} media."
        ));
    }
    Ok(())
}

fn validate_file_header(
    path: &Path,
    kind: MediaKind,
    mime_type: Option<&str>,
) -> Result<(), String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("Could not inspect downloaded media: {error}"))?;
    let mut header = [0_u8; 16];
    let count = file
        .read(&mut header)
        .map_err(|error| format!("Could not inspect downloaded media: {error}"))?;
    let header = &header[..count];
    let valid = match mime_type {
        Some("image/png") => header.starts_with(b"\x89PNG\r\n\x1a\n"),
        Some("image/jpeg") => header.starts_with(&[0xff, 0xd8, 0xff]),
        Some("image/webp") => {
            header.starts_with(b"RIFF") && header.get(8..12) == Some(&b"WEBP"[..])
        }
        Some("video/mp4") => header.get(4..8) == Some(&b"ftyp"[..]),
        Some("audio/wav") => header.starts_with(b"RIFF") && header.get(8..12) == Some(&b"WAVE"[..]),
        Some("audio/mpeg") => {
            header.starts_with(b"ID3")
                || header
                    .get(0..2)
                    .is_some_and(|bytes| bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0)
        }
        _ => false,
    };
    if !valid {
        return Err(format!(
            "Downloaded bytes do not match the declared {kind} MIME type."
        ));
    }
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
        Some("image/png") => String::from("png"),
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
                data: base64::engine::general_purpose::STANDARD
                    .encode(b"\x89PNG\r\n\x1a\nsmall image bytes"),
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

    #[test]
    fn remote_filename_cannot_choose_the_local_path() {
        assert_eq!(
            safe_extension(Some("../../outside.PNG"), Some("image/png")),
            "png"
        );
        let relative = media_relative_path(MediaKind::Image, "safe-id", "png");
        assert_eq!(relative, PathBuf::from("media/images/safe-id.png"));
        assert!(
            !relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        );
    }
}
