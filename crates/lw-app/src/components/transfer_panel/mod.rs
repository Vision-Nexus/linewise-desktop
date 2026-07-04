//! Global transfer panel — the main content area's resident view.
//!
//! Replaces the old single-scope `UploadQueue`. The panel is a pure reader
//! of the resident upload state (`AppState::upload_tasks` + the three
//! progress maps written by `UploadRuntime`); it owns no event pump and no
//! startup recovery. Its structure:
//!
//! * **Primary tabs** `In Progress / Completed / Failed`, counts baked into
//!   the labels (the counts ARE the summary). `Failed` carries a nested
//!   `Quality / Network` split (see `failed.rs`).
//! * **Global by default.** Unlike the old queue it does NOT filter by the
//!   selected scope — uploads from every org are visible at once. An opt-in
//!   "Only {org}/{project}" chip (default OFF) narrows the rendered list to
//!   the selected project when the user wants it. Scope still governs the
//!   *upload target* (`Scope::is_uploadable`), just not the view.
//! * **Toolbar** `[Retry all]` (Network failures only) + `[Clear
//!   completed]`.
//!
//! Staging holds for required capture metadata: a clip that passes QC settles
//! `Staged` and waits. Each row shows "✓ <summary>" once its metadata is set
//! (per-file "Add metadata", or the top-bar batch fill) or "Needs metadata"
//! until then. The single `[Upload N]` button dispatches every filled (ready)
//! clip — its count is the ready count, and it is absent when none are ready.
//!
//! Ingest is consolidated into ONE multi-function "Upload" button in the
//! header: it opens a small menu with "Select files…" (multi-file picker,
//! every pick staged) and "Select folder…" (recursive, videos only).
//! Drag-drop is the third entry and behaves the same (files + folder
//! recursion). There is no sidebar ingest.

mod completed;
mod failed;
mod in_progress;
mod network_chip;
mod rows;
mod tabs;

/// Marker the `DuplicateDetected` reconcile writes into a task's
/// `error_message` so the Completed view renders an "Already exists" badge
/// instead of treating the row as failed. Re-exported here so the resident
/// event handler in `upload_runtime.rs` and the Completed view share one
/// definition.
pub use completed::ALREADY_EXISTS_MARKER;

use crate::components::capture_dialog::CaptureMetadataDialog;
use crate::components::transcode_dialog::TranscodeDialog;
use crate::state::{AppState, CoreServices, ToastKind};
use crate::styles;
use completed::CompletedList;
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use failed::FailedList;
use in_progress::InProgressList;
use lw_core::error::UploadError;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload;
use lw_core::video;
use lw_core::video::DeviceEncoderSignature;
use network_chip::NetworkChip;
use std::path::{Path, PathBuf};
use tabs::{PrimaryTab, PrimaryTabButton, TRANSFER_TAB_CSS};

/// Stage every video under `dir` into `(tenant_id, project_id)`.
///
/// The folder-ingest routine shared by the header "Upload" menu's "Select
/// folder…" choice and the drag-drop path. The recursive walk runs off the
/// UI thread via `spawn_blocking` (`collect_videos_in_dir` is synchronous
/// `std::fs`); then each video is staged via `stage_files`. A folder with no
/// videos shows an info toast so the click doesn't feel like a no-op.
async fn stage_folder(
    engine: std::sync::Arc<lw_core::upload::UploadEngine>,
    dir: PathBuf,
    tenant_id: String,
    project_id: String,
    mut app_state_for_toast: AppState,
) {
    let walk_dir = dir.clone();
    let videos =
        match tokio::task::spawn_blocking(move || upload::collect_videos_in_dir(&walk_dir)).await {
            Ok(videos) => videos,
            Err(join_err) => {
                tracing::error!(dir = %dir.display(), "folder walk task panicked: {join_err}");
                app_state_for_toast.show_toast(
                    "Failed to scan the selected folder".to_string(),
                    ToastKind::Error,
                );
                return;
            }
        };
    tracing::info!(dir = %dir.display(), video_count = videos.len(), "folder picker staged videos");
    if videos.is_empty() {
        app_state_for_toast.show_toast(
            "No videos found in the selected folder".to_string(),
            ToastKind::Info,
        );
        return;
    }
    stage_files(engine, videos, tenant_id, project_id, app_state_for_toast).await;
}

/// Stage each path in `paths` into `(tenant_id, project_id)`, one at a time,
/// surfacing a toast for any staging error.
///
/// Shared by the header "Upload" menu's "Select files…" choice (explicit
/// picks — every selected path is staged as-is) and by `stage_folder` (the
/// recursive video walk). Each file goes through `stage_file`; a failure logs
/// at the typed error's level and shows a per-file error toast, then the loop
/// continues so one bad file doesn't abort the rest of the batch.
async fn stage_files(
    engine: std::sync::Arc<lw_core::upload::UploadEngine>,
    paths: Vec<PathBuf>,
    tenant_id: String,
    project_id: String,
    mut app_state_for_toast: AppState,
) {
    for path in paths {
        if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
            e.log("Stage file");
            app_state_for_toast.show_toast(stage_error_toast(&path, &e), ToastKind::Error);
        }
    }
}

/// Formats a staging error into a user-facing toast string. Expected
/// rejections (the user picked a file we can't accept) get a "Cannot
/// upload" prefix and the typed error's `Display` as the reason.
/// Unexpected failures (network, IO, API, DB) get a "Failed to add"
/// prefix — they look the same to the user, but the prefix matches the
/// log-level distinction in `UploadError::log`.
///
/// Note: most of these variants no longer reach this function because
/// `stage_file` now defers the quality check (broken file, unsupported
/// container, server offline) into a `Rejected` row whose reason renders
/// inline in the panel. The toast path stays exhaustive so that adding
/// a new `UploadError` still forces a deliberate routing decision, but
/// in practice only `FileNotFound`, `Io`, and `Database` arrive here.
fn stage_error_toast(path: &Path, err: &UploadError) -> String {
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    match err {
        UploadError::VideoUnplayable { .. }
        | UploadError::Duplicate { .. }
        | UploadError::DuplicateOnServer { .. }
        | UploadError::FileTooLarge { .. }
        | UploadError::FileNotFound(_)
        | UploadError::Cancelled
        | UploadError::QualityCheckPayloadTooLarge { .. }
        | UploadError::FileChangedDuringUpload { .. }
        | UploadError::SourceFileMissing { .. } => {
            format!("Cannot upload \"{filename}\": {err}")
        }
        UploadError::UnsupportedContainer { kind } => {
            format!(
                "Cannot upload \"{filename}\" ({label}): {err}",
                label = kind.human_label(),
            )
        }
        UploadError::QualityCheckOffline { .. } => {
            format!(
                "Cannot upload \"{filename}\": server unreachable — quality check requires a network connection"
            )
        }
        // The multipart variants only arise during the upload stage, not
        // staging, so they never actually reach this toast path — but the
        // match stays exhaustive, so route them with the other unexpected
        // failures.
        UploadError::Api { .. }
        | UploadError::Auth { .. }
        | UploadError::GcsUpload { .. }
        | UploadError::MpuMissingEtag { .. }
        | UploadError::MpuTaskFailed { .. }
        | UploadError::MpuResumeFailed { .. }
        | UploadError::Network(_)
        | UploadError::Io(_)
        | UploadError::CaptureEmbed { .. }
        | UploadError::Database(_) => format!("Failed to add \"{filename}\": {err}"),
    }
}

#[component]
pub fn TransferPanel() -> Element {
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    // Shared progress maps owned by the resident `UploadRuntime`. `Signal<T>`
    // is Copy, so these are cheap handles passed down to the rows.
    let transcode_progress = app_state.transcode_progress;
    let upload_progress = app_state.upload_progress;
    let hash_progress = app_state.hash_progress;
    let embed_progress = app_state.embed_progress;
    // UI-derived per-task upload speed (bytes/sec), written by the resident
    // `UploadRuntime`. Read-only here — threaded down to the uploading rows.
    let upload_speed = app_state.upload_speed;

    let device_encoder_signatures: &'static [DeviceEncoderSignature] =
        video::device_encoder_signatures();
    let transcode_config = app_state.config.read().transcode.clone();

    // Transcode dialog state: Some(task_id) = dialog open for that task.
    let mut transcode_dialog_task: Signal<Option<String>> = use_signal(|| None);

    // Primary tab — process-local view state, defaults to In Progress.
    let mut tab = use_signal(|| PrimaryTab::InProgress);

    // Per-project filter chip. Default OFF → the panel is global (every org's
    // tasks). When ON and a project is selected, the rendered list narrows to
    // that (tenant, project). This is the only place selection touches the
    // view; the default stays global.
    let mut only_selected = use_signal(|| false);

    // Scope still drives the upload TARGET decision (and the chip label), even
    // though it no longer filters the view by default.
    let scope = app_state.scope();
    let can_upload = scope.is_uploadable();

    // Selected (tenant, project) ids for the opt-in narrowing + chip label.
    let selected_tenant_id = app_state
        .selected_tenant
        .read()
        .as_ref()
        .map(|t| t.id.clone());
    let selected_project_id = app_state
        .selected_project
        .read()
        .as_ref()
        .map(|p| p.id.clone());
    let chip_label = match (
        app_state.selected_tenant.read().as_ref(),
        app_state.selected_project.read().as_ref(),
    ) {
        (Some(tenant), Some(project)) => {
            format!("Only {} / {}", tenant.display_name, project.name)
        }
        _ => "Only selected project".to_string(),
    };
    // The chip only makes sense when a project is selected; otherwise there is
    // nothing to narrow to and we keep the panel global.
    let chip_applicable = can_upload;
    let narrowing = *only_selected.read() && chip_applicable;

    // The full task list (global). Optionally narrowed to the selected project.
    let all_tasks = app_state.upload_tasks.read();
    let tasks: Vec<UploadTask> = match (narrowing, &selected_tenant_id, &selected_project_id) {
        (true, Some(tid), Some(pid)) => all_tasks
            .iter()
            .filter(|t| &t.tenant_id == tid && &t.project_id == pid)
            .cloned()
            .collect(),
        _ => all_tasks.iter().cloned().collect(),
    };
    drop(all_tasks);

    // Per-tab counts — computed once from the (possibly narrowed) list and
    // baked into the tab labels.
    let in_progress_count = tasks
        .iter()
        .filter(|t| PrimaryTab::InProgress.contains(&t.state))
        .count();
    let completed_count = tasks
        .iter()
        .filter(|t| PrimaryTab::Completed.contains(&t.state))
        .count();
    let failed_count = tasks
        .iter()
        .filter(|t| PrimaryTab::Failed.contains(&t.state))
        .count();

    // "Ready" = staged AND its required capture metadata is RESOLVED (filled or
    // skipped). The manual Upload only dispatches these (`confirm_staged` skips the
    // rest), so the button counts and gates on ready, not on all staged. With auto-
    // advance, resolved non-transcode clips leave `Staged` on their own, so this
    // count is mostly the transcode-held clips and the brief pre-advance window.
    // Reading `capture_rev` keeps the count live as the user fills/skips clips.
    let _ = app_state.capture_rev.read();
    let staged_count = tasks
        .iter()
        .filter(|t| {
            t.state == UploadState::Staged && services.upload_engine.capture_resolved(&t.id)
        })
        .count();

    // === Staging + action callbacks ===

    // Open/close state for the header "Upload" menu (file-or-folder choices).
    let mut upload_menu_open = use_signal(|| false);

    let engine_for_files = services.upload_engine.clone();
    let engine_for_folder = services.upload_engine.clone();
    let engine_for_drop = services.upload_engine.clone();

    // "Select files…" — multi-select file picker. These are explicit picks,
    // so every chosen path is staged as-is (no video sniff). Stages via the
    // shared `stage_files` loop so error toasts match every other entry.
    let app_state_files = app_state.clone();
    let on_pick_files = move |_| {
        upload_menu_open.set(false);
        let engine = engine_for_files.clone();
        let tenant_id = app_state_files
            .selected_tenant
            .read()
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        let project_id = app_state_files
            .selected_project
            .read()
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let app_state_for_toast = app_state_files.clone();

        spawn(async move {
            let files = rfd::AsyncFileDialog::new()
                .set_title("Select videos to upload")
                .pick_files()
                .await;
            let Some(files) = files else { return };
            let paths: Vec<PathBuf> = files.iter().map(|f| PathBuf::from(f.path())).collect();
            stage_files(engine, paths, tenant_id, project_id, app_state_for_toast).await;
        });
    };

    // "Select folder…" — folder picker, recursive, videos only (via
    // `stage_folder` / `collect_videos_in_dir`).
    let app_state_folder = app_state.clone();
    let on_pick_folder = move |_| {
        upload_menu_open.set(false);
        let engine = engine_for_folder.clone();
        let tenant_id = app_state_folder
            .selected_tenant
            .read()
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        let project_id = app_state_folder
            .selected_project
            .read()
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let app_state_for_toast = app_state_folder.clone();

        spawn(async move {
            let folder = rfd::AsyncFileDialog::new()
                .set_title("Select a folder of videos to upload")
                .pick_folder()
                .await;
            let Some(folder) = folder else { return };
            stage_folder(
                engine,
                PathBuf::from(folder.path()),
                tenant_id,
                project_id,
                app_state_for_toast,
            )
            .await;
        });
    };

    // Confirm upload (manual). With auto-upload (PR3) this only acts on the
    // clips held `Staged` for an opt-in transcode — everything else already
    // auto-advanced at QC-pass time. Dispatch is bounded-parallel only; the
    // old sequential "one by one" mode is gone.
    let engine_for_confirm = services.upload_engine.clone();
    let app_state_for_confirm = app_state.clone();
    let app_state_confirm = app_state.clone();
    let confirm_cb = use_callback(move |_: ()| {
        let engine = engine_for_confirm.clone();
        let mut app_state_write = app_state_confirm.clone();
        let transcode_ids: Vec<String> = app_state_for_confirm
            .upload_tasks
            .read()
            .iter()
            .filter(|t| t.state == UploadState::Staged && t.transcode)
            .map(|t| t.id.clone())
            .collect();
        spawn(async move {
            match engine.confirm_staged(&transcode_ids).await {
                Ok(ids) => {
                    tracing::info!("Confirmed {} files", ids.len());
                    let mut tasks = app_state_write.upload_tasks.write();
                    for task in tasks.iter_mut() {
                        if ids.contains(&task.id) {
                            task.state = UploadState::Pending;
                        }
                    }
                }
                Err(e) => tracing::error!("Failed to confirm uploads: {e}"),
            }
        });
    });

    let engine_for_remove = services.upload_engine.clone();
    let mut app_state_remove = app_state.clone();
    let on_remove = move |task_id: String| {
        let engine = engine_for_remove.clone();
        spawn(async move {
            if let Err(e) = engine.remove_staged(&task_id).await {
                tracing::error!("Failed to remove staged file: {e}");
            }
            app_state_remove
                .upload_tasks
                .write()
                .retain(|t| t.id != task_id);
        });
    };

    let mut app_state_transcode = app_state.clone();
    let on_transcode_click = move |task_id: String| {
        let is_enabled = app_state_transcode
            .upload_tasks
            .read()
            .iter()
            .any(|t| t.id == task_id && t.transcode);
        if is_enabled {
            let mut tasks = app_state_transcode.upload_tasks.write();
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                task.transcode = false;
            }
        } else {
            transcode_dialog_task.set(Some(task_id));
        }
    };

    let engine_for_retry = services.upload_engine.clone();
    let db_for_retry = services.db.clone();
    let mut app_state_retry = app_state.clone();
    let on_retry = move |task_id: String| {
        let engine = engine_for_retry.clone();
        let db = db_for_retry.clone();
        spawn(async move {
            // Manual retry: zero the durable auto-retry count so a row that hit
            // the give-up cap (`GaveUp`) or accumulated retries starts its budget
            // fresh and re-enters the normal auto-retry flow instead of giving up
            // again on the first failure.
            if let Err(e) = db.reset_retry_count(&task_id).await {
                tracing::warn!("Failed to reset retry_count on manual retry of {task_id}: {e}");
            }
            let _ = db
                .update_upload_state(&task_id, UploadState::Pending, None)
                .await;
            let mut tasks = app_state_retry.upload_tasks.write();
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                task.state = UploadState::Pending;
                task.error_message = None;
                task.retry_count = 0;
                let task = task.clone();
                drop(tasks);
                // Dispatch through the bounded, single-flight path (never a bare
                // process_task spawn): a manual retry must not exceed
                // max_concurrent or double-drive a row already in flight.
                engine.dispatch_one(task);
            }
        });
    };

    // Retry every Network failure at once. Reuses the single-row retry path
    // for each `Failed` row currently rendered.
    let on_retry_all = {
        let on_retry = on_retry.clone();
        let app_state_retry_all = app_state.clone();
        move |_| {
            let failed_ids: Vec<String> = app_state_retry_all
                .upload_tasks
                .read()
                .iter()
                .filter(|t| matches!(t.state, UploadState::Failed | UploadState::GaveUp))
                .map(|t| t.id.clone())
                .collect();
            for id in failed_ids {
                on_retry(id);
            }
        }
    };

    // Force upload (super-admin bypass) for a Rejected row.
    let engine_for_force = services.upload_engine.clone();
    let mut app_state_force = app_state.clone();
    let on_force_upload = move |task_id: String| {
        let engine = engine_for_force.clone();
        let mut tasks = app_state_force.upload_tasks.write();
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.state = UploadState::Pending;
            task.error_message = None;
            task.force_upload = true;
        }
        drop(tasks);
        spawn(async move {
            if let Err(e) = engine.force_upload(&task_id).await {
                e.log(format_args!("Force-upload spawn for {task_id}"));
            }
        });
    };

    let engine_for_pause = services.upload_engine.clone();
    let mut app_state_pause = app_state.clone();
    let on_pause = move |task_id: String| {
        // Instant feedback: mark the row "Pausing…" right away (rows render a
        // disabled "Pausing…" instead of the Pause button). We deliberately do NOT
        // set Paused here — a real Paused only arrives from the engine's
        // StateChanged{Paused}. Any engine state event, or a no-op pause_task,
        // clears this transient marker, so the UI can never claim Paused the engine
        // didn't actually do (no "shows Resume but engine never paused").
        app_state_pause.pausing.write().insert(task_id.clone());
        let engine = engine_for_pause.clone();
        let mut app_state_after = app_state_pause.clone();
        spawn(async move {
            match engine.pause_task(&task_id).await {
                // Engine accepted: the worker will stop at the next chunk/part
                // boundary and emit StateChanged{Paused}; upload_runtime clears the
                // marker and flips the row to Paused (Resume button) then.
                Ok(true) => {}
                // No-op: the row already left Uploading (finished/failed just now).
                // Drop the transient marker — the engine's own event drives the row.
                Ok(false) => {
                    app_state_after.pausing.write().remove(&task_id);
                }
                Err(e) => {
                    tracing::warn!("pause_task({task_id}) failed: {e}");
                    app_state_after.pausing.write().remove(&task_id);
                }
            }
        });
    };

    let engine_for_resume = services.upload_engine.clone();
    let db_for_resume = services.db.clone();
    let mut app_state_resume = app_state.clone();
    let on_resume = move |task_id: String| {
        let engine = engine_for_resume.clone();
        let db = db_for_resume.clone();
        spawn(async move {
            let _ = db
                .update_upload_state(&task_id, UploadState::Pending, None)
                .await;
            let mut tasks = app_state_resume.upload_tasks.write();
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                task.state = UploadState::Pending;
                task.error_message = None;
                let task = task.clone();
                drop(tasks);
                // Same bounded, single-flight dispatch as on_retry — spawning
                // process_task directly here would bypass the upload_semaphore
                // and single-flight guard.
                engine.dispatch_one(task);
            }
        });
    };

    let mut app_state_clear = app_state.clone();
    let engine_for_clear = services.upload_engine.clone();
    let on_clear = move |task_id: String| {
        let engine = engine_for_clear.clone();
        spawn(async move {
            // Abort any in-flight GCS resumable session / incomplete MPU before
            // dropping the row, so removing a partially-uploaded failure doesn't
            // orphan an upload on the server. Completed rows skip the abort
            // inside the engine; the abort is best-effort and never blocks the
            // local delete.
            if let Err(e) = engine.abort_in_flight_and_remove(&task_id).await {
                tracing::error!("Failed to remove upload {task_id}: {e}");
            }
            app_state_clear
                .upload_tasks
                .write()
                .retain(|t| t.id != task_id);
        });
    };

    // Clear every Completed row (delete from DB + UI). Reuses the single-row
    // clear path for each `Completed` row.
    let on_clear_completed = {
        let on_clear = on_clear.clone();
        let app_state_clear_all = app_state.clone();
        move |_| {
            let completed_ids: Vec<String> = app_state_clear_all
                .upload_tasks
                .read()
                .iter()
                .filter(|t| t.state == UploadState::Completed)
                .map(|t| t.id.clone())
                .collect();
            for id in completed_ids {
                on_clear(id);
            }
        }
    };

    // === Drag-and-drop staging ===
    // Dropped FILES stage as before; a dropped DIRECTORY recurses for videos
    // via the same `stage_folder` / `collect_videos_in_dir` path the folder
    // pickers use (item 7), so dropping a folder of clips behaves identically
    // to picking it.
    let mut is_dragging = use_signal(|| false);
    let app_state_drop = app_state.clone();
    let on_drop = move |evt: DragEvent| {
        is_dragging.set(false);
        if !can_upload {
            return;
        }
        let engine = engine_for_drop.clone();
        let tenant_id = app_state_drop
            .selected_tenant
            .read()
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        let project_id = app_state_drop
            .selected_project
            .read()
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let mut app_state_for_toast = app_state_drop.clone();

        let mut files_to_stage: Vec<PathBuf> = Vec::new();
        let mut dirs_to_walk: Vec<PathBuf> = Vec::new();
        let mut skipped: u32 = 0;
        for file in evt.files() {
            let path = file.path();
            if path.as_os_str().is_empty() {
                continue;
            }
            // Directory first: a dropped folder recurses regardless of how its
            // name's extension would sniff (a `.mov` bundle is still a folder).
            if path.is_dir() {
                dirs_to_walk.push(path);
            } else if upload::looks_like_video(&path) {
                files_to_stage.push(path);
            } else {
                skipped += 1;
            }
        }

        if skipped > 0 {
            let label = if skipped == 1 { "file" } else { "files" };
            app_state_for_toast.show_toast(
                format!("Skipped {skipped} non-video {label}"),
                ToastKind::Info,
            );
        }
        if files_to_stage.is_empty() && dirs_to_walk.is_empty() {
            return;
        }

        spawn(async move {
            for path in files_to_stage {
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    e.log("Stage dropped file");
                    app_state_for_toast.show_toast(stage_error_toast(&path, &e), ToastKind::Error);
                }
            }
            for dir in dirs_to_walk {
                stage_folder(
                    engine.clone(),
                    dir,
                    tenant_id.clone(),
                    project_id.clone(),
                    app_state_for_toast.clone(),
                )
                .await;
            }
        });
    };

    let dragging_active = *is_dragging.read() && can_upload;
    let drop_border = if dragging_active {
        "3px dashed var(--border-focus)"
    } else {
        "2px dashed transparent"
    };
    let drop_background = if dragging_active {
        "var(--bg-tertiary)"
    } else {
        "transparent"
    };

    let active_tab = *tab.read();

    // Capture-metadata sheet state. `capture_open` drives visibility (one
    // mounted sheet, for the slide animation); `capture_task` selects the mode:
    // `None` = batch defaults (header button), `Some(id)` = per-file fill (row
    // button), which releases that held clip on save.
    let mut capture_open = use_signal(|| false);
    let mut capture_task: Signal<Option<String>> = use_signal(|| None);

    let on_fill_metadata = move |task_id: String| {
        capture_task.set(Some(task_id));
        capture_open.set(true);
    };

    // Skip capture metadata for one clip: resolves the metadata gate without
    // values and auto-advances the clip to upload (the engine holds it only if
    // it's transcode-eligible). Bump `capture_rev` so the row's "skipped" note and
    // the ready-count re-render immediately (the engine's capture maps aren't
    // reactive). The auto-advance dispatch happens inside the engine.
    let engine_for_skip = services.upload_engine.clone();
    let app_state_skip = app_state.clone();
    let on_skip_metadata = move |task_id: String| {
        let engine = engine_for_skip.clone();
        let mut capture_rev = app_state_skip.capture_rev;
        spawn(async move {
            engine.skip_capture_and_advance(&task_id).await;
            capture_rev += 1;
        });
    };

    rsx! {
        style { "{TRANSFER_TAB_CSS}" }
        div {
            style: "padding: 16px; border: {drop_border}; border-radius: 8px; min-height: 300px; background: {drop_background}; transition: border 0.15s, background 0.15s;",
            ondragover: move |evt| { evt.prevent_default(); is_dragging.set(true); },
            ondragleave: move |_| is_dragging.set(false),
            ondrop: on_drop,

            // Header: title + the single multi-function Upload button (its menu
            // offers files-or-folder) + the held-transcode "Upload N" button.
            div {
                style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 12px;",
                div {
                    style: "display: flex; align-items: center; gap: 10px;",
                    h2 { style: "margin: 0; font-size: 16px;", "Transfers" }
                    // Signal-strength chip — renders nothing until the first probe.
                    NetworkChip {}
                }
                div {
                    style: "display: flex; gap: 8px; align-items: center;",
                    if can_upload {
                        // The one ingest entry: a button that opens a small menu
                        // with "Select files…" and "Select folder…". A
                        // full-screen backdrop closes the menu on outside click,
                        // mirroring the `UserMenu` popover in `title_bar.rs`.
                        div {
                            style: "position: relative;",
                            button {
                                class: "btn-primary",
                                style: "{styles::BTN_PRIMARY}",
                                title: "Upload videos to this project",
                                onclick: move |_| {
                                    let next = !*upload_menu_open.read();
                                    upload_menu_open.set(next);
                                },
                                "Upload"
                            }
                            if *upload_menu_open.read() {
                                div {
                                    style: "position: fixed; inset: 0; z-index: 40;",
                                    onclick: move |_| upload_menu_open.set(false),
                                }
                                div {
                                    style: "position: absolute; top: 100%; right: 0; margin-top: 4px; \
                                            min-width: 180px; z-index: 50; \
                                            background: var(--bg); border: 1px solid var(--border); \
                                            border-radius: 6px; box-shadow: 0 4px 12px rgba(0,0,0,0.15); \
                                            padding: 4px;",
                                    button {
                                        class: "lw-upload-menu-item",
                                        onclick: on_pick_files,
                                        "Select files…"
                                    }
                                    button {
                                        class: "lw-upload-menu-item",
                                        onclick: on_pick_folder,
                                        "Select folder…"
                                    }
                                }
                            }
                        }
                        button {
                            style: "padding: 7px 14px; border-radius: 6px; border: 1px solid var(--border); background: transparent; color: var(--text); cursor: pointer; font-size: 13px;",
                            title: "Set capture metadata for all queued files and any you add next",
                            onclick: move |_| {
                                capture_task.set(None);
                                capture_open.set(true);
                            },
                            "Capture metadata"
                        }
                        if staged_count > 0 {
                            UploadButton { staged_count, confirm_cb }
                        }
                    } else {
                        button {
                            style: "{styles::BTN_DISABLED}",
                            disabled: true,
                            title: "Select a project in the sidebar to enable uploading",
                            "Upload"
                        }
                    }
                }
            }

            // Primary tab strip + per-project chip + toolbar.
            div {
                style: "display: flex; align-items: center; justify-content: space-between; border-bottom: 1px solid var(--border); margin-bottom: 16px; flex-wrap: wrap; gap: 8px;",
                div {
                    style: "display: flex; align-items: center;",
                    PrimaryTabButton {
                        label: "In Progress".to_string(),
                        count: in_progress_count,
                        active: active_tab == PrimaryTab::InProgress,
                        onclick: move |_| tab.set(PrimaryTab::InProgress),
                    }
                    PrimaryTabButton {
                        label: "Completed".to_string(),
                        count: completed_count,
                        active: active_tab == PrimaryTab::Completed,
                        onclick: move |_| tab.set(PrimaryTab::Completed),
                    }
                    PrimaryTabButton {
                        label: "Failed".to_string(),
                        count: failed_count,
                        active: active_tab == PrimaryTab::Failed,
                        onclick: move |_| tab.set(PrimaryTab::Failed),
                    }
                }
                div {
                    style: "display: flex; align-items: center; gap: 8px; padding-bottom: 6px;",
                    if chip_applicable {
                        button {
                            class: if narrowing { "lw-subtab is-active" } else { "lw-subtab" },
                            title: "Show only this project's transfers. Off shows every org.",
                            onclick: move |_| {
                                let on = *only_selected.read();
                                only_selected.set(!on);
                            },
                            "{chip_label}"
                        }
                    }
                    if active_tab == PrimaryTab::Failed {
                        button {
                            class: "btn-outline",
                            style: "height: 26px; padding: 0 10px; font-size: 12px; border-radius: 4px; cursor: pointer; background: transparent; color: var(--text); border: 1px solid var(--border);",
                            title: "Retry all network failures",
                            onclick: on_retry_all,
                            "Retry all"
                        }
                    }
                    if active_tab == PrimaryTab::Completed {
                        button {
                            class: "btn-outline",
                            style: "height: 26px; padding: 0 10px; font-size: 12px; border-radius: 4px; cursor: pointer; background: transparent; color: var(--text-muted); border: 1px solid var(--border);",
                            title: "Remove all completed rows from history",
                            onclick: on_clear_completed,
                            "Clear completed"
                        }
                    }
                }
            }

            // Tab body. Only one arm renders per pass, so each consumes the
            // single owned `tasks` Vec by value (move, not clone) — the counts
            // above already finished borrowing it, and nothing below the match
            // touches it.
            match active_tab {
                PrimaryTab::InProgress => rsx! {
                    InProgressList {
                        tasks,
                        transcode_config: transcode_config.clone(),
                        device_encoder_signatures,
                        transcode_progress,
                        upload_progress,
                        hash_progress,
                        embed_progress,
                        upload_speed,
                        on_remove: on_remove.clone(),
                        on_clear: on_clear.clone(),
                        on_transcode_click,
                        on_fill_metadata,
                        on_skip_metadata,
                        on_retry: on_retry.clone(),
                        on_pause: on_pause.clone(),
                        on_resume: on_resume.clone(),
                    }
                },
                PrimaryTab::Completed => rsx! {
                    CompletedList { tasks }
                },
                PrimaryTab::Failed => rsx! {
                    FailedList {
                        tasks,
                        transcode_config: transcode_config.clone(),
                        device_encoder_signatures,
                        transcode_progress,
                        upload_progress,
                        upload_speed,
                        on_remove: on_remove.clone(),
                        on_clear: on_clear.clone(),
                        on_transcode_click,
                        on_retry: on_retry.clone(),
                        on_pause: on_pause.clone(),
                        on_resume: on_resume.clone(),
                        on_force_upload,
                    }
                },
            }

            // Transcode config sheet. Mounted unconditionally so the slide-out
            // animation can play on close; visibility is controlled by `open`.
            {
                let mut app_state_dialog = app_state.clone();
                let dialog_task = transcode_dialog_task.read().clone();
                rsx! {
                    TranscodeDialog {
                        task_id: dialog_task.clone().unwrap_or_default(),
                        open: dialog_task.is_some(),
                        on_close: move |enabled: bool| {
                            if enabled
                                && let Some(tid) = transcode_dialog_task.read().clone()
                            {
                                let mut tasks = app_state_dialog.upload_tasks.write();
                                if let Some(task) = tasks.iter_mut().find(|t| t.id == tid) {
                                    task.transcode = true;
                                }
                            }
                            transcode_dialog_task.set(None);
                        },
                    }
                }
            }

            // Capture-metadata sheet (batch or per-file by `capture_task`).
            // Mounted unconditionally for the slide animation; visibility driven
            // by `capture_open`.
            CaptureMetadataDialog {
                open: capture_open(),
                task_id: capture_task(),
                on_close: move |_saved: bool| {
                    capture_open.set(false);
                    capture_task.set(None);
                },
            }
        }
    }
}

/// The Upload button: a single primary action that dispatches every `Staged`
/// clip whose required capture metadata is set (bounded-parallel). `staged_count`
/// is the READY count (filled clips); the button is absent when it is 0, so the
/// user fills at least one clip before it appears. Clips still missing metadata
/// stay `Staged` and are skipped by `confirm_staged`.
#[component]
fn UploadButton(staged_count: usize, confirm_cb: Callback<()>) -> Element {
    let label = if staged_count == 1 {
        format!("Upload {staged_count} file")
    } else {
        format!("Upload {staged_count} files")
    };

    rsx! {
        button {
            class: "btn-success",
            style: "{styles::BTN_SUCCESS}",
            onclick: move |_| confirm_cb.call(()),
            "{label}"
        }
    }
}
