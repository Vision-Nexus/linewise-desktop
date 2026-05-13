use std::path::PathBuf;

use crate::config::AppConfig;

/// EnvFilter directive used as the built-in default and as the value
/// written into a fresh `config.toml`. `lw_*` crates run at trace so
/// packaged binaries capture rich diagnostics for our own code, while
/// noisy dependencies (wry, hyper, sqlx, …) stay at info.
///
/// Note: EnvFilter matches against Rust module paths, which use
/// underscores — `lw_app`, not `lw-app`. Renaming a crate breaks this
/// silently because EnvFilter does not error on unknown targets.
pub const DEFAULT_LOG_FILTER: &str = "info,lw_app=trace,lw_core=trace,lw_chat=trace";

/// Filename prefix used by the rolling appender. Kept here so the
/// "find the latest log file" logic stays in sync with the writer.
pub const LOG_FILENAME_PREFIX: &str = "linewise-desktop";
pub const LOG_FILENAME_SUFFIX: &str = "log";

pub fn log_dir() -> PathBuf {
    AppConfig::data_dir().join("logs")
}

pub fn ensure_log_dir() -> std::io::Result<PathBuf> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the path of the most recently modified rotated log file in
/// `log_dir`, or `None` if the directory does not exist or contains no
/// matching files. Useful for a future "send logs to support" affordance
/// that needs to find what the appender is currently writing into —
/// the actual filename is `linewise-desktop.<YYYY-MM-DD>.log`, not a
/// fixed name, so callers can't hard-code it.
pub fn latest_log_path() -> std::io::Result<Option<PathBuf>> {
    let dir = log_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(LOG_FILENAME_PREFIX) || !name.ends_with(LOG_FILENAME_SUFFIX) {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if newest.as_ref().is_none_or(|(_, t)| modified > *t) {
            newest = Some((path, modified));
        }
    }
    Ok(newest.map(|(p, _)| p))
}
