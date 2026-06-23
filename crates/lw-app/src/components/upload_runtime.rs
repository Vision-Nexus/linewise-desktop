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
//! The progress maps (transcode / upload / hash / capture-embed) live in
//! [`AppState`] (not here), so the transfer view can read them without owning
//! the pump. This component is the only writer of those maps.

use crate::components::transfer_panel::ALREADY_EXISTS_MARKER;
use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload::UploadEvent;
use std::collections::HashMap;

/// Upper bound on how many events one drain round applies in a single
/// synchronous batch. A staging burst (100 files → hundreds of events) is
/// coalesced into one re-render per batch; this cap keeps a pathological flood
/// from starving the renderer for an unbounded stretch — any leftovers drain on
/// the next round.
const MAX_EVENT_BATCH: usize = 512;

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
            // Reclaim orphaned desensitize temp copies from a prior crash /
            // hard-kill (the in-process Drop guard can't run on a kill). Age-gated
            // (>30 min) so a concurrent — single-instance-guard-bypassed — instance's
            // in-flight copy isn't deleted mid-upload; run BEFORE resume so this
            // instance's about-to-be-rebuilt copies are never targeted.
            lw_core::desensitize::sweep_orphaned_temp(std::time::Duration::from_secs(30 * 60));

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
            // Load history. `get_all_uploads` returns newest-first
            // (created_at DESC); reverse to oldest-first so the in-memory queue
            // is in add-time order. New rows are appended at the end by the
            // `TaskAdded` handler, so the whole list stays chronological and
            // every tab renders rows in the order files were added.
            match db.get_all_uploads().await {
                Ok(mut tasks) if !tasks.is_empty() => {
                    tracing::info!("Loaded {} upload tasks from history", tasks.len());
                    tasks.reverse();
                    app_state.upload_tasks.set(tasks);
                }
                Err(e) => tracing::warn!("Failed to load upload history: {e}"),
                _ => {}
            }
            // Capture state is in-memory and lost on restart, but the tags live in
            // the files. Read them back for staged clips so a previously-filled row
            // shows "✓ filled" (and uploads) instead of falsely demanding metadata.
            // Bump the UI revision so the recovered rows re-render.
            if engine.recover_capture_for_staged().await {
                app_state.capture_rev += 1;
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
        let mut upload_speed = app_state.upload_speed;
        // Per-task speed sampling state, LOCAL to this drain loop (never a
        // signal): `(last_instant, last_bytes, ema_bps)`. The engine's
        // `Progress` event carries no timestamp, so we time successive events
        // here and EMA-smooth the instantaneous rate before publishing it to
        // `upload_speed` for the rows to read.
        let mut speed_samples: HashMap<String, (std::time::Instant, u64, f64)> = HashMap::new();
        async move {
            loop {
                // Block for at least one event, then drain everything already
                // queued into one batch. Staging 100 files fires a burst of
                // events (TaskAdded ×N, StateChanged, HashProgress); handling
                // them one-await-per-event made Dioxus re-render once per
                // event. Applying the whole burst synchronously (no `.await`
                // between the writes below) lets Dioxus coalesce the signal
                // writes into a single re-render per batch.
                let mut batch = Vec::new();
                {
                    let mut rx = event_rx.lock().await;
                    match rx.recv().await {
                        Some(ev) => batch.push(ev),
                        None => break,
                    }
                    while batch.len() < MAX_EVENT_BATCH {
                        match rx.try_recv() {
                            Ok(ev) => batch.push(ev),
                            // Empty (nothing more queued right now) or
                            // Disconnected — stop draining this round. A
                            // disconnect is observed on the next `recv().await`
                            // above, which returns `None` and breaks the loop.
                            Err(_) => break,
                        }
                    }
                } // lock released before processing — never held across a write

                // Process the whole batch synchronously. No `.await` inside
                // this loop, so all the signal writes coalesce into one render.
                for event in batch {
                    // PEEK by reference before the event is moved into
                    // `handle_upload_event` — that function stays as-is (no
                    // speed params). This match is a filter, not a state
                    // machine, so a `_ =>` catch-all is fine here.
                    match &event {
                        UploadEvent::Progress {
                            task_id,
                            bytes_uploaded,
                            ..
                        } => {
                            sample_upload_speed(
                                &mut speed_samples,
                                &mut upload_speed,
                                task_id,
                                *bytes_uploaded,
                            );
                        }
                        UploadEvent::Completed { task_id }
                        | UploadEvent::Failed { task_id, .. }
                        | UploadEvent::DuplicateDetected { task_id, .. } => {
                            speed_samples.remove(task_id);
                            upload_speed.write().remove(task_id);
                        }
                        _ => {}
                    }
                    handle_upload_event(
                        &mut app_state,
                        &mut transcode_progress,
                        &mut upload_progress,
                        &mut hash_progress,
                        event,
                    );
                }
            }
        }
    });

    rsx! {}
}

/// Smallest gap between two `Progress` events that counts as a fresh speed
/// sample. Below this the divide is noisy (and risks blowing up as `dt → 0`),
/// so we skip and wait for the next event.
const MIN_SAMPLE_INTERVAL_SECS: f64 = 0.2;
/// EMA weight on the newest instantaneous rate; `1 - ALPHA` stays on the prior
/// average. 0.3 favours stability over snappiness — fine at chunk granularity.
const SPEED_EMA_ALPHA: f64 = 0.3;

/// Fold one `Progress` observation into the per-task EMA rate and publish it.
///
/// `samples` holds `(last_instant, last_bytes, ema_bps)` per task, owned by the
/// drain loop. The first observation for a task seeds the baseline and emits
/// nothing (we have no interval yet). Subsequent observations compute an
/// instantaneous bytes/sec over the elapsed wall-clock time and blend it into
/// the EMA, which is then written to `upload_speed`.
///
/// Guards: a sub-threshold interval is ignored (too noisy); a byte count that
/// regresses is ignored without disturbing the baseline — GCS resumable
/// retries can reset `bytes_uploaded` mid-stream, and a negative delta is not a
/// real rate.
fn sample_upload_speed(
    samples: &mut HashMap<String, (std::time::Instant, u64, f64)>,
    upload_speed: &mut Signal<HashMap<String, f64>>,
    task_id: &str,
    bytes_uploaded: u64,
) {
    let now = std::time::Instant::now();
    let Some(&(last_t, last_bytes, prev_ema)) = samples.get(task_id) else {
        // First sample for this task — seed the baseline, emit nothing yet.
        samples.insert(task_id.to_string(), (now, bytes_uploaded, 0.0));
        return;
    };
    // Skip resumable byte-counter resets: a regression is not a real rate, and
    // we keep the existing baseline so the next forward delta measures cleanly.
    if bytes_uploaded < last_bytes {
        return;
    }
    let dt = now.duration_since(last_t).as_secs_f64();
    if dt < MIN_SAMPLE_INTERVAL_SECS {
        return;
    }
    let inst_bps = (bytes_uploaded - last_bytes) as f64 / dt;
    let ema = if prev_ema <= 0.0 {
        inst_bps
    } else {
        SPEED_EMA_ALPHA * inst_bps + (1.0 - SPEED_EMA_ALPHA) * prev_ema
    };
    samples.insert(task_id.to_string(), (now, bytes_uploaded, ema));
    upload_speed.write().insert(task_id.to_string(), ema);
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
            // A capture-embed bar only makes sense while a row is `Staged`; drop
            // any lingering entry once it advances (or is rejected), so a missed
            // completion tick can never leave a stuck bar on a moved row.
            if state != UploadState::Staged {
                app_state.embed_progress.write().remove(&task_id);
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
        UploadEvent::CaptureEmbedProgress {
            task_id,
            bytes,
            total,
        } => {
            // Drop the bar on completion (final tick sends bytes == total) or when
            // the size is unknown; otherwise show the determinate rewrite progress.
            if total == 0 || bytes >= total {
                app_state.embed_progress.write().remove(&task_id);
            } else {
                app_state
                    .embed_progress
                    .write()
                    .insert(task_id, (bytes, total));
            }
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
                // The event carries a plain `Option<VideoInfo>`; the stored
                // field is `Arc`-wrapped so later render-time task clones are
                // cheap. Wrap on the way into the store.
                t.video_info = video_info.map(std::sync::Arc::new);
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
        UploadEvent::DuplicateDetected {
            task_id,
            existing_document_id,
        } => {
            // A duplicate means the content is already stored on the server —
            // that is success from the user's standpoint, not a failure. Land
            // it under Completed with an "Already exists" marker (read by the
            // Completed view via `ALREADY_EXISTS_MARKER`) instead of in
            // Failed/Network, where a `[Retry]` would be wrong (retrying just
            // re-detects the same dup). We reuse `error_message` as the badge
            // source rather than touch the engine state machine or DB schema.
            // The existing document id is recorded on the row so the completed
            // detail refers to the server-side copy that already exists.
            upload_progress.write().remove(&task_id);
            transcode_progress.write().remove(&task_id);
            hash_progress.write().remove(&task_id);
            update_task(app_state, &task_id, |t| {
                t.state = UploadState::Completed;
                t.error_message = Some(ALREADY_EXISTS_MARKER.to_string());
                t.document_id = Some(existing_document_id.clone());
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
