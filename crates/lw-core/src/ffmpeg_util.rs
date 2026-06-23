//! Shared ffmpeg helpers used by the PDQ frame-hash and transcode paths:
//! locating the bundled ffmpeg binary, spawning it without flashing a console
//! window on Windows, and cleaning up a temp output file.

use std::ffi::OsString;
use std::path::Path;

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
/// spawns while hashing). No-op on non-Windows.
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

/// Remove a temp output file produced by the transcode path (best-effort).
pub fn cleanup_temp_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!("Failed to clean up temp file {}: {e}", path.display());
    }
}
