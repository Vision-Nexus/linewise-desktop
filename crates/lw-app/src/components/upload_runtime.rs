//! Resident upload runtime — the process-global owner of the upload event
//! pump and the one-shot startup recovery.
//!
//! This component is mounted once, high in the tree (inside `AuthedShell`,
//! above the login/main navigation), and never unmounts while a
//! `CoreServices` is live. It renders nothing. Its whole job is to host two
//! hooks that MUST NOT be tied to a view that mounts/unmounts on navigation:
//!
//! 1. **The single event-drain loop.** `CoreServices::event_rx` is an
//!    `UnboundedReceiver` behind a mutex — exactly ONE task may own `recv()`.
//!    If this loop lived in a view that remounts (the old `UploadQueue`), a
//!    second mount would either split events across two consumers or drop the
//!    relay. Keeping it resident guarantees a single consumer for the life of
//!    the session.
//! 2. **One-shot startup recovery** (reset stale rows → resume pending → load
//!    history). This runs once per `CoreServices` init, not once per view
//!    entry. Re-running `reset_stale_uploads` on a navigation would flip live
//!    in-flight rows to FAILED.
//!
//! The three progress maps live in [`AppState`] (not here), so the transfer
//! view can read them without owning the pump. This component is the only
//! writer of those maps.

use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload::UploadEvent;
use std::collections::HashMap;

#[component]
pub fn UploadRuntime() -> Element {
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    // One-shot startup recovery: reset stale in-progress rows, resume
    // resumable work, then load history. Runs once when this runtime mounts
    // (i.e. once per CoreServices init) — NOT per navigation.
    let app_state_load = app_state.clone();
    let db_for_load = services.db.clone();
    let engine_for_load = services.upload_engine.clone();
    use_future(move || {
        let db = db_for_load.clone();
        let engine = engine_for_load.clone();
        let mut app_state = app_state_load.clone();
        async move {
            // Reset stale in-progress uploads to FAILED. Does NOT touch
            // TRANSCODING — that state is resumable via the scratch dir.
            match db.reset_stale_uploads().await {
                Ok(n) if n > 0 => tracing::info!("Reset {n} stale uploads to FAILED"),
                Err(e) => tracing::warn!("Failed to reset stale uploads: {e}"),
                _ => {}
            }
            // Resume any task left in a resumable state (PENDING, TRANSCODING,
            // UPLOADING, etc). Without this, killed-mid-transcode tasks sit
            // in the queue at 0% forever because nothing drives them forward.
            if let Err(e) = engine.resume_pending().await {
                tracing::warn!("Failed to resume pending uploads: {e}");
            }
            // Load history
            match db.get_all_uploads().await {
                Ok(tasks) if !tasks.is_empty() => {
                    tracing::info!("Loaded {} upload tasks from history", tasks.len());
                    app_state.upload_tasks.set(tasks);
                }
                Err(e) => tracing::warn!("Failed to load upload history: {e}"),
                _ => {}
            }
        }
    });

    // The single event-drain loop — the ONLY consumer of `event_rx`. Resident
    // so it never stops or duplicates across view/org switches. Progress maps
    // live in `AppState`; this loop is their sole writer.
    let app_state_events = app_state.clone();
    let event_rx = services.event_rx.clone();
    use_future(move || {
        let event_rx = event_rx.clone();
        let mut app_state = app_state_events.clone();
        let mut transcode_progress = app_state.transcode_progress;
        let mut upload_progress = app_state.upload_progress;
        let mut hash_progress = app_state.hash_progress;
        async move {
            loop {
                let event = {
                    let mut rx = event_rx.lock().await;
                    rx.recv().await
                };
                let Some(event) = event else { break };
                handle_upload_event(
                    &mut app_state,
                    &mut transcode_progress,
                    &mut upload_progress,
                    &mut hash_progress,
                    event,
                );
            }
        }
    });

    rsx! {}
}

/// Apply one engine event to the UI state: task list + progress maps.
///
/// Moved here from `upload_queue.rs` together with the pump so the view layer
/// no longer carries event-handling logic. `app_state.upload_tasks` carries
/// task identity/state; the three progress signals carry byte-level progress
/// (kept separate so a `Progress` tick doesn't re-render the whole list).
fn handle_upload_event(
    app_state: &mut AppState,
    transcode_progress: &mut Signal<HashMap<String, f32>>,
    upload_progress: &mut Signal<HashMap<String, (u64, u64)>>,
    hash_progress: &mut Signal<HashMap<String, (u64, u64)>>,
    event: UploadEvent,
) {
    match event {
        UploadEvent::TaskAdded(task) => {
            app_state.upload_tasks.write().push(*task);
        }
        UploadEvent::StateChanged { task_id, state } => {
            // Drop any in-flight hash bar once the row leaves Hashing —
            // otherwise a freshly-Staged row keeps a stale 100% bar
            // entry until the next add/drop.
            if state != UploadState::Hashing {
                hash_progress.write().remove(&task_id);
            }
            update_task(app_state, &task_id, |t| t.state = state);
        }
        UploadEvent::Progress {
            task_id,
            bytes_uploaded,
            total_bytes,
        } => {
            // Monotonic clamp: never let the displayed bytes drop below the
            // highest value we've already seen for this task. GCS resumable
            // retries legitimately reset the byte counter mid-stream, but the
            // wire-side progress never regresses — acknowledged bytes stay
            // acknowledged across sessions.
            let mut guard = upload_progress.write();
            let entry = guard.entry(task_id).or_insert((0, total_bytes));
            entry.0 = entry.0.max(bytes_uploaded);
            entry.1 = total_bytes;
        }
        UploadEvent::HashProgress {
            task_id,
            bytes_hashed,
            total_bytes,
        } => {
            hash_progress
                .write()
                .insert(task_id, (bytes_hashed, total_bytes));
        }
        UploadEvent::ValidationWarnings {
            task_id,
            warnings,
            rejection_reasons,
        } => {
            update_task(app_state, &task_id, |t| {
                t.validation_warnings = warnings;
                t.rejection_reasons = rejection_reasons;
            });
        }
        UploadEvent::QualityCheckPassed {
            task_id,
            video_info,
            warnings,
        } => {
            update_task(app_state, &task_id, |t| {
                t.video_info = video_info;
                t.validation_warnings = warnings;
            });
        }
        UploadEvent::TranscodeProgress { task_id, percent } => {
            transcode_progress.write().insert(task_id, percent);
        }
        UploadEvent::TranscodeCompleted {
            task_id,
            transcoded_size,
        } => {
            transcode_progress.write().remove(&task_id);
            update_task(app_state, &task_id, |t| {
                t.transcoded_size = Some(transcoded_size)
            });
        }
        UploadEvent::DuplicateDetected { task_id, .. } => {
            upload_progress.write().remove(&task_id);
            transcode_progress.write().remove(&task_id);
            hash_progress.write().remove(&task_id);
            update_task(app_state, &task_id, |t| {
                t.state = UploadState::Failed;
                t.error_message = Some("Duplicate file detected".to_string());
            });
        }
        UploadEvent::Completed { task_id } => {
            upload_progress.write().remove(&task_id);
            transcode_progress.write().remove(&task_id);
            hash_progress.write().remove(&task_id);
            update_task(app_state, &task_id, |t| t.state = UploadState::Completed);
        }
        UploadEvent::Failed { task_id, error } => {
            upload_progress.write().remove(&task_id);
            transcode_progress.write().remove(&task_id);
            hash_progress.write().remove(&task_id);
            update_task(app_state, &task_id, |t| {
                t.state = UploadState::Failed;
                t.error_message = Some(error);
            });
        }
    }
}

fn update_task(app_state: &mut AppState, task_id: &str, f: impl FnOnce(&mut UploadTask)) {
    let mut tasks = app_state.upload_tasks.write();
    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
        f(task);
    }
}
