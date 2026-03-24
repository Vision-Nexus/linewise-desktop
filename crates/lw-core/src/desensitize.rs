//! Data desensitization — strip metadata from files before cross-border upload.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum DesensitizeError {
    #[error("ffmpeg not found in PATH")]
    FfmpegNotFound,
    #[error("ffmpeg failed: {0}")]
    FfmpegFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of desensitization — path to the cleaned file
#[derive(Debug)]
pub struct DesensitizeResult {
    /// Path to the desensitized file (in temp dir)
    pub output_path: PathBuf,
    /// Whether any metadata was actually stripped
    pub metadata_stripped: bool,
}

/// Strip all metadata from a video file using ffmpeg.
/// Creates a new file in a temp directory with metadata removed.
/// Uses `-c copy` for fast remuxing (no re-encoding).
pub async fn strip_video_metadata(input: &Path) -> Result<DesensitizeResult, DesensitizeError> {
    let input = input.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let temp_dir = std::env::temp_dir().join("linewise-desensitize");
        std::fs::create_dir_all(&temp_dir)?;

        let filename = input
            .file_name()
            .expect("input must have filename")
            .to_string_lossy();
        let output = temp_dir.join(format!("clean_{filename}"));

        let result = Command::new("ffmpeg")
            .args([
                "-y", // overwrite output
                "-i",
            ])
            .arg(&input)
            .args([
                "-map_metadata",
                "-1", // strip all global metadata
                "-map_chapters",
                "-1", // strip chapter metadata
                "-c",
                "copy", // no re-encoding, fast copy
                "-movflags",
                "+faststart",
            ])
            .arg(&output)
            .output();

        match result {
            Ok(out) if out.status.success() => Ok(DesensitizeResult {
                output_path: output,
                metadata_stripped: true,
            }),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(DesensitizeError::FfmpegFailed(stderr.to_string()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(DesensitizeError::FfmpegNotFound)
            }
            Err(e) => Err(DesensitizeError::Io(e)),
        }
    })
    .await
    .expect("strip_video_metadata task panicked")
}

/// Strip EXIF/metadata from an image file using ffmpeg.
pub async fn strip_image_metadata(input: &Path) -> Result<DesensitizeResult, DesensitizeError> {
    let input = input.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let temp_dir = std::env::temp_dir().join("linewise-desensitize");
        std::fs::create_dir_all(&temp_dir)?;

        let filename = input
            .file_name()
            .expect("input must have filename")
            .to_string_lossy();
        let output = temp_dir.join(format!("clean_{filename}"));

        let result = Command::new("ffmpeg")
            .args(["-y", "-i"])
            .arg(&input)
            .args(["-map_metadata", "-1"])
            .arg(&output)
            .output();

        match result {
            Ok(out) if out.status.success() => Ok(DesensitizeResult {
                output_path: output,
                metadata_stripped: true,
            }),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(DesensitizeError::FfmpegFailed(stderr.to_string()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(DesensitizeError::FfmpegNotFound)
            }
            Err(e) => Err(DesensitizeError::Io(e)),
        }
    })
    .await
    .expect("strip_image_metadata task panicked")
}

/// Strip metadata from any supported file based on MIME type.
/// Returns None if the file type doesn't need desensitization.
pub async fn strip_metadata(
    input: &Path,
    mime_type: &str,
) -> Option<Result<DesensitizeResult, DesensitizeError>> {
    if mime_type.starts_with("video/") {
        Some(strip_video_metadata(input).await)
    } else if mime_type.starts_with("image/") {
        Some(strip_image_metadata(input).await)
    } else {
        None
    }
}

/// Clean up desensitized temp files
pub fn cleanup_temp_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!("Failed to clean up temp file {}: {e}", path.display());
    }
}
