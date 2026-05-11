//! File system watcher — monitors directories for new files and queues them for upload.

use crate::config::WatchFolderEntry;
use notify_debouncer_mini::{DebouncedEventKind, new_debouncer};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

/// Events emitted when the watcher detects new files
#[derive(Debug, Clone)]
pub struct WatchEvent {
    pub path: PathBuf,
    pub tenant_id: String,
    pub project_id: String,
}

/// Spawns file watchers for all configured watch folders.
/// Returns a receiver that emits WatchEvents when new files are detected.
pub fn start_watching(
    folders: Vec<WatchFolderEntry>,
) -> Result<mpsc::UnboundedReceiver<WatchEvent>, notify::Error> {
    let (tx, rx) = mpsc::unbounded_channel();

    for folder in folders {
        let tx = tx.clone();
        let tenant_id = folder.tenant_id.clone();
        let project_id = folder.project_id.clone();
        let file_filter = folder.file_filter.clone();
        let path = folder.path.clone();

        std::thread::spawn(move || {
            let rt_tx = tx;
            let (notify_tx, notify_rx) = std::sync::mpsc::channel();

            let mut debouncer = match new_debouncer(Duration::from_secs(2), notify_tx) {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("Failed to create watcher for {}: {e}", path.display());
                    return;
                }
            };

            if let Err(e) = debouncer
                .watcher()
                .watch(&path, notify::RecursiveMode::Recursive)
            {
                tracing::error!("Failed to watch {}: {e}", path.display());
                return;
            }

            tracing::info!("Watching folder: {}", path.display());

            for result in notify_rx {
                let events = match result {
                    Ok(events) => events,
                    Err(e) => {
                        tracing::warn!("Watch error on {}: {e:?}", path.display());
                        continue;
                    }
                };

                let new_files = events.iter().filter(|e| {
                    e.kind == DebouncedEventKind::Any
                        && !e.path.is_dir()
                        && matches_filter(&e.path, &file_filter)
                });

                for event in new_files {
                    tracing::info!("New file detected: {}", event.path.display());
                    let _ = rt_tx.send(WatchEvent {
                        path: event.path.clone(),
                        tenant_id: tenant_id.clone(),
                        project_id: project_id.clone(),
                    });
                }
            }
        });
    }

    Ok(rx)
}

/// Check if a file matches the configured MIME type filters.
/// Filters like "video/*", "application/pdf", "image/*"
fn matches_filter(path: &PathBuf, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();

    filters.iter().any(|filter| {
        if let Some(prefix) = filter.strip_suffix("/*") {
            mime.starts_with(prefix)
        } else {
            mime == *filter
        }
    })
}
