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
#[tracing::instrument(skip_all, fields(filename = input.file_name().and_then(|s| s.to_str()).unwrap_or("?")))]
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
pub(crate) fn resolve_ffmpeg_binary() -> OsString {
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

/// Build a `Command` for an ffmpeg-CLI invocation that does NOT pop a console
/// window on Windows. The desktop app runs on the `windows` GUI subsystem;
/// spawning a console subprocess without `CREATE_NO_WINDOW` flashes a black
/// `cmd` window per spawn — very visible during PDQ frame extraction (up to 5
/// spawns while hashing) and image desensitization. No-op on non-Windows.
pub(crate) fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    // `&mut cmd` is taken on every platform (so `mut` is always "used" — no
    // `unused_mut` — and the binding isn't a bare `let x; x` — no
    // `let_and_return`), but the flag is only set on Windows.
    apply_no_console_window(&mut cmd);
    cmd
}

/// Set `CREATE_NO_WINDOW` so a spawned console subprocess has no window.
#[cfg(windows)]
fn apply_no_console_window(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// No-op off Windows — there is no console-window concept to suppress.
#[cfg(not(windows))]
fn apply_no_console_window(_cmd: &mut std::process::Command) {}

/// Strip EXIF/metadata from an image file.
/// Still uses ffmpeg CLI for images (ffmpeg-next's image handling is less ergonomic).
#[tracing::instrument(skip_all, fields(filename = input.file_name().and_then(|s| s.to_str()).unwrap_or("?")))]
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

        let result = hidden_command(resolve_ffmpeg_binary())
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
#[tracing::instrument(skip_all, fields(mime_type = %mime_type))]
pub async fn strip_metadata(
    input: &Path,
    mime_type: &str,
) -> Option<Result<DesensitizeResult, DesensitizeError>> {
    if mime_type.starts_with("video/") {
        tracing::info!("dispatching video desensitize");
        Some(strip_video_metadata(input).await)
    } else if mime_type.starts_with("image/") {
        tracing::info!("dispatching image desensitize");
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

/// Directory holding desensitized `clean_<filename>` copies (system %TEMP%).
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("linewise-desensitize")
}

/// Reclaim orphaned desensitized temp copies left behind when a prior upload
/// failed / was cancelled / the app was killed before the in-process cleanup
/// ran (a hard kill / power loss can't run a `Drop` guard). Only entries OLDER
/// than `max_age` are removed, so an in-flight copy from a concurrent
/// (single-instance-guard-bypassed) instance sharing %TEMP% is never deleted
/// mid-upload. Best-effort: every error is logged and skipped. Call once at
/// startup, BEFORE resuming pending uploads, so this instance's
/// about-to-be-rebuilt copies are never targeted.
pub fn sweep_orphaned_temp(max_age: std::time::Duration) {
    let dir = temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return; // dir absent (nothing ever desensitized) — nothing to reclaim
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0u64;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        // `duration_since` errs if mtime is in the future (clock skew) — keep it.
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < max_age {
            continue;
        }
        let path = entry.path();
        let file_bytes = if meta.is_file() { meta.len() } else { 0 };
        let result = if meta.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => {
                removed += 1;
                bytes += file_bytes;
            }
            Err(e) => tracing::warn!("temp sweep: failed to remove {}: {e}", path.display()),
        }
    }
    if removed > 0 {
        tracing::info!(
            "temp sweep: reclaimed {removed} orphaned desensitize entr(ies) (~{} MiB) from {}",
            bytes / 1_048_576,
            dir.display()
        );
    }
}
