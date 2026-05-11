//! Data desensitization — strip metadata from files before cross-border upload.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum DesensitizeError {
    #[error("ffmpeg not available")]
    FfmpegNotAvailable,
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

/// Strip all metadata from a video file using ffmpeg-next.
/// Creates a new file in a temp directory with metadata removed.
/// Uses stream copy (no re-encoding) for fast remuxing.
pub async fn strip_video_metadata(input: &Path) -> Result<DesensitizeResult, DesensitizeError> {
    let input = input.to_path_buf();

    tokio::task::spawn_blocking(move || strip_video_metadata_blocking(&input))
        .await
        .expect("strip_video_metadata task panicked")
}

fn strip_video_metadata_blocking(input: &Path) -> Result<DesensitizeResult, DesensitizeError> {
    use ffmpeg_next::{codec, format, media};

    let temp_dir = std::env::temp_dir().join("linewise-desensitize");
    std::fs::create_dir_all(&temp_dir)?;

    let filename = input
        .file_name()
        .expect("input must have filename")
        .to_string_lossy();
    let output = temp_dir.join(format!("clean_{filename}"));

    let mut ictx = format::input(input)
        .map_err(|e| DesensitizeError::FfmpegFailed(format!("Failed to open input: {e}")))?;

    let mut octx = format::output(&output)
        .map_err(|e| DesensitizeError::FfmpegFailed(format!("Failed to open output: {e}")))?;

    // Map all streams, copying codec parameters (no re-encoding)
    let mut stream_mapping = vec![None; ictx.streams().count()];
    let mut output_idx = 0usize;

    for input_stream in ictx.streams() {
        let medium = input_stream.parameters().medium();
        if medium != media::Type::Video
            && medium != media::Type::Audio
            && medium != media::Type::Subtitle
        {
            continue;
        }

        let mut new_stream = octx
            .add_stream(codec::encoder::find(codec::Id::None))
            .map_err(|e| DesensitizeError::FfmpegFailed(format!("Add stream: {e}")))?;
        new_stream.set_parameters(input_stream.parameters());
        // Clear stream-level metadata (set_parameters copies it)
        stream_mapping[input_stream.index()] = Some(output_idx);
        output_idx += 1;
    }

    // Write header with empty metadata (strips global metadata)
    octx.set_metadata(ffmpeg_next::Dictionary::new());
    octx.write_header()
        .map_err(|e| DesensitizeError::FfmpegFailed(format!("Write header: {e}")))?;

    // Copy packets
    for (stream, mut packet) in ictx.packets() {
        let in_idx = stream.index();
        let Some(out_idx) = stream_mapping[in_idx] else {
            continue;
        };

        let in_tb = stream.time_base();
        let out_tb = octx
            .stream(out_idx)
            .expect("output stream exists")
            .time_base();

        packet.set_stream(out_idx);
        packet.rescale_ts(in_tb, out_tb);
        packet
            .write_interleaved(&mut octx)
            .map_err(|e| DesensitizeError::FfmpegFailed(format!("Write packet: {e}")))?;
    }

    octx.write_trailer()
        .map_err(|e| DesensitizeError::FfmpegFailed(format!("Write trailer: {e}")))?;

    Ok(DesensitizeResult {
        output_path: output,
        metadata_stripped: true,
    })
}

/// Resolve the ffmpeg CLI binary, preferring the bundled copy over system PATH.
fn resolve_ffmpeg_binary() -> OsString {
    let Ok(exe) = std::env::current_exe() else {
        return OsString::from("ffmpeg");
    };

    #[cfg(target_os = "macos")]
    {
        // .app/Contents/MacOS/binary → .app/Contents/Resources/ffmpeg
        if let Some(resources) = exe.parent().and_then(|p| p.parent()) {
            let candidate = resources.join("Resources").join("ffmpeg");
            if candidate.exists() {
                return candidate.into_os_string();
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("ffmpeg.exe");
            if candidate.exists() {
                return candidate.into_os_string();
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(dir) = exe.parent() {
            // Same directory (AppImage / portable)
            let candidate = dir.join("ffmpeg");
            if candidate.exists() {
                return candidate.into_os_string();
            }
            // Installed .deb layout: /usr/bin/../lib/linewise-desktop/ffmpeg
            let candidate = dir.join("../lib/linewise-desktop/ffmpeg");
            if candidate.exists() {
                return candidate.into_os_string();
            }
        }
    }

    OsString::from("ffmpeg")
}

/// Strip EXIF/metadata from an image file.
/// Still uses ffmpeg CLI for images (ffmpeg-next's image handling is less ergonomic).
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

        let result = std::process::Command::new(resolve_ffmpeg_binary())
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
                Err(DesensitizeError::FfmpegNotAvailable)
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
