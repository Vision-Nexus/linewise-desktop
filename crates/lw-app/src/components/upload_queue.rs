use crate::components::progress::{Progress, ProgressIndicator};
use crate::components::transcode_dialog::TranscodeDialog;
use crate::state::{AppState, CoreServices, ToastKind};
use crate::styles;
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use lw_core::config::TranscodeConfig;
use lw_core::error::UploadError;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload;
use lw_core::video;
use lw_core::video::DeviceEncoderSignature;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
/// inline in the queue. The toast path stays exhaustive so that adding
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
        | UploadError::QualityCheckPayloadTooLarge { .. } => {
            format!("Cannot upload \"{filename}\": {err}")
        }
        // Pull the kind-specific label into the prefix so the toast reads
        // "Cannot upload \"clip.mkv\" (matroska): Linewise supports..."
        // instead of just "Cannot upload \"clip.mkv\": Linewise supports...".
        // The user spotted the format from the file extension; surfacing
        // the detected kind alongside it makes the rejection feel less
        // like a guess.
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
        UploadError::Api { .. }
        | UploadError::Auth { .. }
        | UploadError::GcsUpload { .. }
        | UploadError::Network(_)
        | UploadError::Io(_)
        | UploadError::Database(_) => format!("Failed to add \"{filename}\": {err}"),
    }
}

#[component]
pub fn UploadQueue() -> Element {
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    // Startup recovery (reset stale / resume / load history) and the event
    // pump now live in the resident `UploadRuntime` (see upload_runtime.rs),
    // so they run once per session and survive view/org switches. This view
    // only READS the shared progress maps from AppState. `Signal<T>` is Copy,
    // so these are cheap handles passed down to the rows.
    let transcode_progress = app_state.transcode_progress;
    let upload_progress = app_state.upload_progress;
    let hash_progress = app_state.hash_progress;

    // Transcode dialog state: Some(task_id) = dialog open for that task
    let mut transcode_dialog_task: Signal<Option<String>> = use_signal(|| None);

    // Scope drives everything visible below. `All` shows every task,
    // `Tenant` narrows to one org, `Project` narrows to one (org, project)
    // pair — and is the only scope in which staging new uploads makes
    // sense (the engine needs both ids). This replaces the old
    // `has_context` boolean, which couldn't express the tenant-only view
    // and left the queue empty on sign-in.
    let scope = app_state.scope();
    let can_upload = scope.is_uploadable();

    let all_tasks = app_state.upload_tasks.read();
    let tasks: Vec<UploadTask> = all_tasks
        .iter()
        .filter(|t| scope.matches(&t.tenant_id, &t.project_id))
        .cloned()
        .collect();

    let staged_count = tasks
        .iter()
        .filter(|t| t.state == UploadState::Staged)
        .count();
    let _active_count = tasks.iter().filter(|t| t.state.is_active()).count();

    // Device-encoder vendor signatures used by the popover to split a
    // bare encoder string ("DJI Osmo Nano") into Make + Model. Lives in
    // a tiny `'static` table now that the rule loader is gone — passing
    // the slice straight through avoids any per-row allocation.
    let device_encoder_signatures: &'static [DeviceEncoderSignature] =
        video::device_encoder_signatures();

    // Human-readable label for the Add Files button, e.g. "Add Files to
    // Acme / Website". Only rendered when `can_upload`, so both reads are
    // guaranteed to be `Some` in practice — still, fall back gracefully.
    let add_files_label = match (
        app_state.selected_tenant.read().as_ref(),
        app_state.selected_project.read().as_ref(),
    ) {
        (Some(tenant), Some(project)) => {
            format!("Add Files to {} / {}", tenant.display_name, project.name)
        }
        _ => "Add Files".to_string(),
    };

    // Stage files (step 1)
    let engine_for_add = services.upload_engine.clone();
    let engine_for_drop = services.upload_engine.clone();

    let app_state_add = app_state.clone();
    let on_add_files = move |_| {
        let engine = engine_for_add.clone();
        let tenant_id = app_state_add
            .selected_tenant
            .read()
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        let project_id = app_state_add
            .selected_project
            .read()
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        let mut app_state_for_toast = app_state_add.clone();

        spawn(async move {
            // Timing-instrumented to chase a 1–3 s freeze right after
            // dismissing the picker. `t_picker_done` is the moment
            // rfd's future resolves; subsequent deltas measure how
            // long each `stage_file` `.await` takes on the dioxus
            // task (which shares the main webview thread on macOS).
            let t_open = std::time::Instant::now();
            let files = rfd::AsyncFileDialog::new()
                .set_title("Select files to upload")
                .pick_files()
                .await;
            let t_picker_done = t_open.elapsed();
            tracing::debug!(
                t_picker_ms = t_picker_done.as_millis() as u64,
                "file picker resolved",
            );
            let Some(files) = files else { return };
            tracing::info!(file_count = files.len(), "file picker returned files");
            for file in files {
                let t_file = std::time::Instant::now();
                let path = PathBuf::from(file.path());
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    e.log("Stage file");
                    app_state_for_toast.show_toast(stage_error_toast(&path, &e), ToastKind::Error);
                }
                tracing::debug!(
                    filename = ?path.file_name(),
                    t_stage_call_ms = t_file.elapsed().as_millis() as u64,
                    "stage_file call returned to UI",
                );
            }
        });
    };

    // Confirm upload (step 2). One callback drives all three triggers (the
    // primary button + the two split-menu items); `sequential` selects the
    // dispatch mode and is remembered (persisted to config) for next time.
    let engine_for_confirm = services.upload_engine.clone();
    let config_for_confirm = services.config.clone();
    let app_state_for_confirm = app_state.clone();
    let app_state_confirm = app_state.clone();
    let confirm_cb = use_callback(move |sequential: bool| {
        let engine = engine_for_confirm.clone();
        let mut app_state_write = app_state_confirm.clone();
        // Collect transcode-opted task IDs from the UI signal.
        let transcode_ids: Vec<String> = app_state_for_confirm
            .upload_tasks
            .read()
            .iter()
            .filter(|t| t.state == UploadState::Staged && t.transcode)
            .map(|t| t.id.clone())
            .collect();
        // Remember the choice for next session (best-effort; disk only — the
        // in-session default is the `upload_seq` signal below).
        let mut cfg = config_for_confirm.clone();
        cfg.upload.sequential_uploads = sequential;
        if let Err(e) = cfg.save() {
            tracing::warn!("Failed to persist upload mode: {e}");
        }
        spawn(async move {
            match engine.confirm_staged(&transcode_ids, sequential).await {
                Ok(ids) => {
                    tracing::info!("Confirmed {} files (sequential={sequential})", ids.len());
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

    // Split "Upload" button state: the remembered dispatch mode (initial value
    // from persisted config) and whether the options dropdown is open.
    let mut upload_seq = use_signal(|| services.config.upload.sequential_uploads);
    let mut show_upload_menu = use_signal(|| false);

    // Remove staged file
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

    // Open transcode dialog or disable transcode for a staged task
    let mut app_state_transcode = app_state.clone();
    let on_transcode_click = move |task_id: String| {
        let is_enabled = app_state_transcode
            .upload_tasks
            .read()
            .iter()
            .any(|t| t.id == task_id && t.transcode);
        if is_enabled {
            // Already enabled → disable
            let mut tasks = app_state_transcode.upload_tasks.write();
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                task.transcode = false;
            }
        } else {
            // Not enabled → open config dialog
            transcode_dialog_task.set(Some(task_id));
        }
    };

    // Retry failed upload
    let engine_for_retry = services.upload_engine.clone();
    let db_for_retry = services.db.clone();
    let mut app_state_retry = app_state.clone();
    let on_retry = move |task_id: String| {
        let engine = engine_for_retry.clone();
        let db = db_for_retry.clone();
        spawn(async move {
            // Reset state to Pending, clear error
            let _ = db
                .update_upload_state(&task_id, lw_core::models::UploadState::Pending, None)
                .await;
            // Update UI
            let mut tasks = app_state_retry.upload_tasks.write();
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                task.state = UploadState::Pending;
                task.error_message = None;
                let mut task = task.clone();
                drop(tasks);
                // Re-process
                let eng = engine.clone();
                tokio::spawn(async move {
                    if let Err(e) = eng.process_task(&mut task).await {
                        e.log(format_args!("Retry of {}", task.filename));
                    }
                });
            }
        });
    };

    // Force upload (super-admin bypass) for a Rejected row. Flips the
    // task's `force_upload` flag, transitions it to PENDING in the DB,
    // and spawns the upload worker — which sees the flag in Stage 1 and
    // skips the local-DB dedup short-circuit. The acceptance gate is
    // not re-run on this path either, so a quality-rejected row also
    // proceeds. The UI gates this affordance on
    // `UserInfo::is_super_admin`, but the engine method itself is not
    // role-checked: the desktop is the only caller and the backend
    // does its own permission checks at create-document / upload time.
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

    // Pause an active upload
    let db_for_pause = services.db.clone();
    let mut app_state_pause = app_state.clone();
    let on_pause = move |task_id: String| {
        let db = db_for_pause.clone();
        spawn(async move {
            let _ = db
                .update_upload_state(&task_id, UploadState::Paused, None)
                .await;
            let mut tasks = app_state_pause.upload_tasks.write();
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                task.state = UploadState::Paused;
            }
        });
    };

    // Resume a paused upload
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
                let mut task = task.clone();
                drop(tasks);
                let eng = engine.clone();
                tokio::spawn(async move {
                    if let Err(e) = eng.process_task(&mut task).await {
                        e.log(format_args!("Resume of {}", task.filename));
                    }
                });
            }
        });
    };

    // Remove from history (delete from DB + UI)
    let mut app_state_clear = app_state.clone();
    let db_for_clear = services.db.clone();
    let on_clear = move |task_id: String| {
        let db = db_for_clear.clone();
        spawn(async move {
            let _ = db.delete_upload_task(&task_id).await;
            app_state_clear
                .upload_tasks
                .write()
                .retain(|t| t.id != task_id);
        });
    };

    // DnD
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

        // Split the drop into video and non-video files at the drop
        // site. Non-videos are reported once via a toast count rather
        // than per-file, so a folder full of stray .DS_Store / .txt
        // doesn't spam the user.
        let mut to_stage: Vec<PathBuf> = Vec::new();
        let mut skipped: u32 = 0;
        for file in evt.files() {
            let path = file.path();
            if path.as_os_str().is_empty() {
                continue;
            }
            if upload::looks_like_video(&path) {
                to_stage.push(path);
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
        if to_stage.is_empty() {
            return;
        }

        spawn(async move {
            for path in to_stage {
                let t_file = std::time::Instant::now();
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    e.log("Stage dropped file");
                    app_state_for_toast.show_toast(stage_error_toast(&path, &e), ToastKind::Error);
                }
                tracing::debug!(
                    filename = ?path.file_name(),
                    t_stage_call_ms = t_file.elapsed().as_millis() as u64,
                    "stage_file (drop) call returned to UI",
                );
            }
        });
    };

    // While a drag is in progress over the queue panel, thicken the
    // dashed border and tint the background so the drop target reads
    // as active. Both cues fade out via the panel's `transition` rule.
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

    let quality_checking: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::QualityChecking)
        .cloned()
        .collect();
    let hashing: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Hashing)
        .cloned()
        .collect();
    let staged: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Staged)
        .cloned()
        .collect();
    let rejected: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Rejected)
        .cloned()
        .collect();
    let transcoding: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Transcoding)
        .cloned()
        .collect();
    // `Paused` rows belong in the active section even though
    // `is_active()` returns false for them — `is_active` means "engine
    // is processing this row," and a paused row isn't. But the user
    // expects to see the row right where they left it, with a Resume
    // button, instead of having it vanish out of every section.
    let active: Vec<_> = tasks
        .iter()
        .filter(|t| {
            (t.state.is_active() || t.state == UploadState::Paused)
                && t.state != UploadState::Transcoding
        })
        .cloned()
        .collect();
    let history: Vec<_> = tasks
        .iter()
        .filter(|t| matches!(t.state, UploadState::Completed | UploadState::Failed))
        .cloned()
        .collect();

    // The dispatch mode (concurrent vs one-by-one) may only change while the
    // queue is idle. Locking it during active/queued/paused work guarantees the
    // sequential drainer and the concurrent fan-out never run at the same time,
    // so they can't both touch the same row. (VLP-747 Part B, decision A.)
    let queue_active = tasks
        .iter()
        .any(|t| t.state.is_active() || t.state == UploadState::Paused);

    rsx! {
        div {
            style: "padding: 16px; border: {drop_border}; border-radius: 8px; min-height: 300px; background: {drop_background}; transition: border 0.15s, background 0.15s;",
            ondragover: move |evt| { evt.prevent_default(); is_dragging.set(true); },
            ondragleave: move |_| is_dragging.set(false),
            ondrop: on_drop,

            // Header
            div {
                style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px;",
                h2 { style: "margin: 0; font-size: 16px;", "Upload Queue" }
                div {
                    style: "display: flex; gap: 8px; align-items: center;",
                    if can_upload {
                        button {
                            class: "btn-primary",
                            style: "{styles::BTN_PRIMARY}",
                            onclick: on_add_files,
                            "{add_files_label}"
                        }
                        if staged_count > 0 {
                            {
                                let label = if staged_count == 1 {
                                    format!("Upload {staged_count} file")
                                } else {
                                    format!("Upload {staged_count} files")
                                };
                                let mode_hint = if *upload_seq.read() { " (one by one)" } else { "" };
                                let menu_title = if queue_active {
                                    "Finish or stop the current uploads to change mode"
                                } else {
                                    "Upload options"
                                };
                                rsx! {
                                    div {
                                        style: "position: relative; display: inline-flex; align-items: center; gap: 2px;",
                                        // Primary action: upload in the remembered mode.
                                        button {
                                            class: "btn-success",
                                            style: "{styles::BTN_SUCCESS}",
                                            onclick: move |_| confirm_cb.call(*upload_seq.read()),
                                            "{label}{mode_hint}"
                                        }
                                        // Dropdown toggle. Locked while uploads are active so the
                                        // dispatch strategy can't change mid-batch (which could let a
                                        // lingering concurrent task and the sequential drainer touch
                                        // the same row). The mode is chosen only when the queue is idle.
                                        button {
                                            class: "btn-success",
                                            style: "padding-left: 9px; padding-right: 9px;",
                                            disabled: queue_active,
                                            title: "{menu_title}",
                                            onclick: move |_| {
                                                if queue_active {
                                                    return;
                                                }
                                                let open = *show_upload_menu.read();
                                                show_upload_menu.set(!open);
                                            },
                                            "▾"
                                        }
                                        if *show_upload_menu.read() && !queue_active {
                                            div {
                                                style: "position: absolute; top: 100%; right: 0; margin-top: 4px; background: var(--bg-tertiary); border: 1px solid var(--border-focus); border-radius: 6px; box-shadow: 0 4px 12px rgba(0,0,0,0.25); z-index: 50; min-width: 220px; overflow: hidden;",
                                                button {
                                                    style: "display: block; width: 100%; text-align: left; padding: 9px 12px; background: transparent; border: none; cursor: pointer; font-size: 13px;",
                                                    onclick: move |_| {
                                                        upload_seq.set(false);
                                                        show_upload_menu.set(false);
                                                        confirm_cb.call(false);
                                                    },
                                                    "Upload all at once (parallel)"
                                                }
                                                button {
                                                    style: "display: block; width: 100%; text-align: left; padding: 9px 12px; background: transparent; border: none; cursor: pointer; font-size: 13px; border-top: 1px solid var(--border-focus);",
                                                    onclick: move |_| {
                                                        upload_seq.set(true);
                                                        show_upload_menu.set(false);
                                                        confirm_cb.call(true);
                                                    },
                                                    "Upload one by one (sequential)"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        button {
                            style: "{styles::BTN_DISABLED}",
                            disabled: true,
                            title: "Select a project in the sidebar to enable uploading",
                            "Add Files"
                        }
                    }
                }
            }

            if !can_upload && tasks.is_empty() {
                div {
                    style: "text-align: center; padding: 48px 16px; color: var(--text-muted); font-size: 14px;",
                    "Select a project in the sidebar to start uploading"
                }
            } else {
                // Staged files (step 1)
                if !quality_checking.is_empty() {
                    SectionHeader { title: "Checking", count: quality_checking.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                        for task in quality_checking.iter() {
                            QualityCheckingRow {
                                key: "{task.id}",
                                task: task.clone(),
                                on_remove: on_remove.clone(),
                            }
                        }
                    }
                }

                if !hashing.is_empty() {
                    SectionHeader { title: "Hashing", count: hashing.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                        for task in hashing.iter() {
                            HashingRow {
                                key: "{task.id}",
                                task: task.clone(),
                                device_encoder_signatures,
                                hash_progress,
                                on_remove: on_remove.clone(),
                            }
                        }
                    }
                }

                if !staged.is_empty() {
                    SectionHeader { title: "Ready to Upload", count: staged.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                        for task in staged.iter() {
                            StagedRow {
                                key: "{task.id}",
                                task: task.clone(),
                                transcode_config: app_state.config.read().transcode.clone(),
                                device_encoder_signatures,
                                on_remove: on_remove.clone(),
                                on_transcode_click,
                                on_force_upload: None,
                            }
                        }
                    }
                }

                // Rejected files — shown separately so the user can tell at a
                // glance which files will not upload. StagedRow already
                // handles the red border, REJECTED chip, and red warning rows.
                if !rejected.is_empty() {
                    SectionHeader { title: "Rejected", count: rejected.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                        for task in rejected.iter() {
                            StagedRow {
                                key: "{task.id}",
                                task: task.clone(),
                                transcode_config: app_state.config.read().transcode.clone(),
                                device_encoder_signatures,
                                on_remove: on_remove.clone(),
                                on_transcode_click,
                                on_force_upload: Some(EventHandler::new(on_force_upload.clone())),
                            }
                        }
                    }
                }

                // Preparing (transcoding)
                if !transcoding.is_empty() {
                    SectionHeader { title: "Preparing", count: transcoding.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                        for task in transcoding.iter() {
                            UploadTaskRow {
                                key: "{task.id}",
                                task: task.clone(),
                                transcode_progress,
                                upload_progress,
                                on_retry: on_retry.clone(),
                                on_remove: on_clear.clone(),
                                on_pause: on_pause.clone(),
                                on_resume: on_resume.clone(),
                            }
                        }
                    }
                }

                // Active uploads
                if !active.is_empty() {
                    SectionHeader { title: "Uploading", count: active.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                        for task in active.iter() {
                            UploadTaskRow {
                                key: "{task.id}",
                                task: task.clone(),
                                transcode_progress,
                                upload_progress,
                                on_retry: on_retry.clone(),
                                on_remove: on_clear.clone(),
                                on_pause: on_pause.clone(),
                                on_resume: on_resume.clone(),
                            }
                        }
                    }
                }

                // History
                if !history.is_empty() {
                    SectionHeader { title: "History", count: history.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px;",
                        for task in history.iter() {
                            UploadTaskRow {
                                key: "{task.id}",
                                task: task.clone(),
                                transcode_progress,
                                upload_progress,
                                on_retry: on_retry.clone(),
                                on_remove: on_clear.clone(),
                                on_pause: on_pause.clone(),
                                on_resume: on_resume.clone(),
                            }
                        }
                    }
                }

                // Empty state. Wording tracks the scope — only prompt for
                // drop/add when the current scope can actually accept new
                // uploads (Project scope).
                if quality_checking.is_empty() && hashing.is_empty() && staged.is_empty() && rejected.is_empty() && transcoding.is_empty() && active.is_empty() && history.is_empty() {
                    div {
                        style: "text-align: center; padding: 48px 16px; color: var(--text-muted);",
                        p { style: "font-size: 14px; margin: 0 0 4px;", "No files in queue" }
                        if can_upload {
                            p { style: "font-size: 13px; margin: 0;", "Drop files here or click \"Add Files\"" }
                        } else {
                            p { style: "font-size: 13px; margin: 0;", "Select a project in the sidebar to start uploading" }
                        }
                    }
                }
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
        }
    }
}

#[component]
fn SectionHeader(title: String, count: usize) -> Element {
    rsx! {
        div {
            style: "display: flex; align-items: center; gap: 8px; margin-bottom: 8px;",
            span { style: "font-size: 13px; font-weight: 600; color: var(--text);", "{title}" }
            span {
                style: "font-size: 11px; color: var(--text-secondary); background: var(--bg-tertiary); padding: 2px 8px; border-radius: 10px;",
                "{count}"
            }
        }
    }
}

/// Row metadata derived from a `VideoInfo`: a one-line `summary` for
/// the dashed-underline affordance and three column groups for the
/// hover popover. Lifted out of `StagedRow` so the same markup can
/// render under both the `Checking → Hashing` rows (where the data
/// has just landed) and the `Staged / Rejected` rows (where it has
/// been there since staging finished).
#[derive(Clone, PartialEq)]
struct VideoDetails {
    summary: String,
    structural: Vec<(String, String)>,
    device: Vec<(String, String)>,
    raw: Vec<(String, String)>,
}

fn build_video_details(
    task: &UploadTask,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
) -> Option<VideoDetails> {
    let info = task.video_info.as_ref()?;
    let codec = info.codec.to_uppercase();
    let res = format!("{}x{}", info.width, info.height);
    let fps_text = format!("{:.0}fps", info.fps);
    let bitrate = video::format_bitrate(info.bitrate_kbps);
    let summary = format!("{codec} · {res} · {fps_text} · {bitrate}");
    let mut structural: Vec<(String, String)> = Vec::new();
    structural.push(("Codec".into(), info.codec.to_uppercase()));
    structural.push(("Resolution".into(), res));
    structural.push(("Frame rate".into(), format!("{:.2} fps", info.fps)));
    structural.push(("Bitrate".into(), bitrate));
    if !info.audio_codec.is_empty() {
        structural.push(("Audio".into(), info.audio_codec.to_uppercase()));
    }
    structural.push(("Duration".into(), format_duration(info.duration_secs)));
    structural.push(("Container".into(), info.format.clone()));

    let mut device: Vec<(String, String)> =
        video::device_info_rows(info, device_encoder_signatures)
            .into_iter()
            .map(|(label, value)| {
                let display = if value.is_empty() {
                    "\u{2014}".to_string()
                } else {
                    value
                };
                (label.to_string(), display)
            })
            .collect();
    device.push((
        "Telemetry".into(),
        info.telemetry.clone().unwrap_or_else(|| "\u{2014}".into()),
    ));

    let raw: Vec<(String, String)> = info.metadata.clone();
    Some(VideoDetails {
        summary,
        structural,
        device,
        raw,
    })
}

/// Renders the dashed-underline summary + the three-group hover
/// popover (source metadata, device, raw tags). Identical in
/// `HashingRow` and `StagedRow`; pulled out so adding a new row that
/// shows probe data doesn't need to copy 60 lines of rsx.
#[component]
fn VideoInfoPopover(details: VideoDetails) -> Element {
    let VideoDetails {
        summary,
        structural,
        device,
        raw,
    } = details;
    rsx! {
        div {
            class: "popover-host",
            style: "margin-top: 4px;",
            tabindex: "0",
            div {
                style: "font-size: 11px; color: var(--text-secondary); border-bottom: 1px dashed var(--border); display: inline-block;",
                "{summary}"
            }
            div {
                class: "popover-panel",
                style: if raw.is_empty() {
                    "max-height: 360px; overflow-y: auto;".to_string()
                } else {
                    "max-height: 360px; overflow-y: auto; display: grid; grid-template-columns: minmax(200px, 1fr) minmax(220px, 1fr); gap: 12px;".to_string()
                },
                div {
                    style: "min-width: 0;",
                    div {
                        style: "font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-bottom: 6px;",
                        "Source metadata"
                    }
                    div {
                        style: "display: grid; grid-template-columns: max-content 1fr; column-gap: 10px; row-gap: 3px; font-size: 11px;",
                        for (key, value) in structural.iter() {
                            div { style: "color: var(--text-muted); white-space: nowrap;", "{key}" }
                            div { style: "color: var(--text); word-break: break-all;", "{value}" }
                        }
                    }
                    div {
                        style: "font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-top: 10px; margin-bottom: 6px;",
                        "Device"
                    }
                    div {
                        style: "display: grid; grid-template-columns: max-content 1fr; column-gap: 10px; row-gap: 3px; font-size: 11px;",
                        for (key, value) in device.iter() {
                            div { style: "color: var(--text-muted); white-space: nowrap;", "{key}" }
                            div { style: "color: var(--text); word-break: break-all;", "{value}" }
                        }
                    }
                }
                if !raw.is_empty() {
                    div {
                        style: "min-width: 0;",
                        div {
                            style: "font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-bottom: 6px;",
                            "Raw tags"
                        }
                        div {
                            style: "display: grid; grid-template-columns: max-content 1fr; column-gap: 10px; row-gap: 3px; font-size: 11px;",
                            for (key, value) in raw.iter() {
                                div { style: "color: var(--text-muted); white-space: nowrap; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;", "{key}" }
                                div { style: "color: var(--text); word-break: break-all;", "{value}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One row in the "Checking" section: a freshly-added video whose
/// local atom walk + server `/quality-check` round-trip is in flight.
/// Renders an indeterminate progress bar — the network round-trip
/// has no progress signal we could surface — plus a Remove
/// affordance. Removing mid-check is safe for the same reason as
/// `HashingRow`: the worker writes to SQLite at completion, so a
/// removed row's terminal write becomes a no-op.
#[component]
fn QualityCheckingRow(task: UploadTask, on_remove: EventHandler<String>) -> Element {
    let task_id = task.id.clone();
    rsx! {
        div {
            class: "staged-row fade-in",
            style: "padding: 10px 12px; border: 1px solid var(--staged-border); border-radius: 6px; background: var(--staged-bg);",
            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                span {
                    style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0;",
                    "{task.filename}"
                }
                span {
                    style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0; margin-left: 8px;",
                    "{format_size(task.size)}"
                }
            }
            div {
                style: "margin-top: 6px;",
                // Passing a `None` value flips dioxus-primitives'
                // Progress into `data-state='indeterminate'`, which
                // the global CSS animates as a left-to-right shimmer.
                Progress {
                    value: Option::<f64>::None,
                    max: 100.0,
                    "aria-label": "Quality check in progress",
                    ProgressIndicator {}
                }
            }
            div {
                style: "font-size: 11px; margin-top: 2px; color: var(--text-muted);",
                "Checking video quality…"
            }
            div {
                style: "display: flex; justify-content: flex-end; margin-top: 6px;",
                button {
                    class: "btn-danger-sm",
                    style: "{styles::BTN_DANGER_SM}",
                    onclick: move |_| on_remove.call(task_id.clone()),
                    "Remove"
                }
            }
        }
    }
}

/// One row in the "Hashing" section: a freshly-added file whose
/// BLAKE3+MD5 stream is in flight. The quality check has already
/// landed, so the row carries the same probe-data popover and
/// advisory warnings the `Staged` row will eventually show — the
/// user gets to see the verdict during the hash window instead of
/// only after it. Renders a determinate progress bar driven by
/// `HashProgress` events, plus a Remove affordance. We let the user
/// remove a row mid-hash — the worker writes to SQLite at
/// completion, so a removed row's terminal write becomes a no-op
/// when the row no longer exists.
#[component]
fn HashingRow(
    task: UploadTask,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
    hash_progress: Signal<HashMap<String, (u64, u64)>>,
    on_remove: EventHandler<String>,
) -> Element {
    let task_id = task.id.clone();
    let (bytes_hashed, total_bytes) = hash_progress
        .read()
        .get(&task.id)
        .copied()
        .unwrap_or((0, task.size.max(1)));
    let pct = if total_bytes > 0 {
        ((bytes_hashed as f64 / total_bytes as f64) * 100.0).min(100.0)
    } else {
        0.0
    };
    let label = format!(
        "Hashing — {} / {} ({:.0}%)",
        format_size(bytes_hashed),
        format_size(total_bytes),
        pct,
    );
    let video_details = build_video_details(&task, device_encoder_signatures);
    let warning_style = "font-size: 11px; color: var(--warning); margin-top: 4px; padding: 3px 6px; background: var(--warning-bg); border-radius: 3px;";

    rsx! {
        div {
            class: "staged-row fade-in",
            style: "padding: 10px 12px; border: 1px solid var(--staged-border); border-radius: 6px; background: var(--staged-bg);",
            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                span {
                    style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0;",
                    "{task.filename}"
                }
                span {
                    style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0; margin-left: 8px;",
                    "{format_size(task.size)}"
                }
            }
            if let Some(details) = video_details {
                VideoInfoPopover { details }
            }
            for warning in task.validation_warnings.iter() {
                div {
                    style: "{warning_style}",
                    "{warning}"
                }
            }
            div {
                style: "margin-top: 6px;",
                Progress {
                    value: pct,
                    max: 100.0,
                    "aria-label": "Hash progress",
                    ProgressIndicator {}
                }
            }
            div {
                style: "font-size: 11px; margin-top: 2px; color: var(--text-muted);",
                "{label}"
            }
            div {
                style: "display: flex; justify-content: flex-end; margin-top: 6px;",
                button {
                    class: "btn-danger-sm",
                    style: "{styles::BTN_DANGER_SM}",
                    onclick: move |_| on_remove.call(task_id.clone()),
                    "Remove"
                }
            }
        }
    }
}

#[component]
fn StagedRow(
    task: UploadTask,
    transcode_config: TranscodeConfig,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
    on_remove: EventHandler<String>,
    on_transcode_click: EventHandler<String>,
    /// Super-admin override hook. `Some` only on rows in the
    /// `Rejected` section; the regular Staged section passes `None`.
    /// Even when present, the button only renders for users whose
    /// `system_roles` contain `admin` (see `UserInfo::is_super_admin`).
    on_force_upload: Option<EventHandler<String>>,
) -> Element {
    let task_id = task.id.clone();
    let force_id = task.id.clone();
    let transcode_id = task.id.clone();
    let is_video = task.mime_type.starts_with("video/");
    let is_super_admin = use_context::<AppState>()
        .user_info
        .read()
        .as_ref()
        .map(|u| u.is_super_admin())
        .unwrap_or(false);
    let transcode_on = task.transcode;
    // Upscale guard: only show the transcode toggle when transcoding would
    // actually shrink this clip. Non-video files never get the toggle; for
    // videos without a probe yet, fall back to showing the toggle (user can
    // still opt out manually).
    let transcode_useful = task
        .video_info
        .as_ref()
        .map(|info| video::transcode_would_help(info, &transcode_config))
        .unwrap_or(true);
    // Master toggle gate: when the feature is disabled in global
    // settings, don't surface the per-task affordance at all. The
    // "Already matches targets" badge follows because it only makes
    // sense relative to a feature the user is using.
    let feature_enabled = transcode_config.enabled;
    let rejected = task.state == UploadState::Rejected;
    let show_transcode_toggle = feature_enabled && is_video && transcode_useful && !rejected;
    let show_already_ok_badge = feature_enabled && is_video && !transcode_useful && !rejected;

    // Probe data is built into the same shape — summary line + three
    // popover groups — used by `HashingRow`. Lifting it out makes the
    // hashing row light up with codec/resolution/fps the moment the
    // quality check returns, instead of waiting for the row to land
    // in `Staged`.
    let video_details = build_video_details(&task, device_encoder_signatures);

    let btn_style = "height: 24px; padding: 0 8px; font-size: 11px; border-radius: 4px; cursor: pointer; border: 1px solid var(--border); transition: background 0.15s;";
    let transcode_btn_style = if transcode_on {
        format!(
            "{btn_style} background: var(--btn-primary); color: white; border-color: var(--btn-primary);"
        )
    } else {
        format!("{btn_style} background: transparent; color: var(--text-secondary);")
    };
    let transcode_label = if transcode_on {
        "Transcode \u{2713}"
    } else {
        "Transcode"
    };

    let is_rejected = task.state == UploadState::Rejected;
    let row_style = if is_rejected {
        "padding: 10px 12px; border: 1px solid var(--error); border-radius: 6px; background: var(--error-bg); transition: background 0.15s, border-color 0.15s;"
    } else {
        "padding: 10px 12px; border: 1px solid var(--staged-border); border-radius: 6px; background: var(--staged-bg); transition: background 0.15s, border-color 0.15s;"
    };
    // Two severities, two palettes:
    //   * `warning_style` — recommend-band advisories, telemetry hints,
    //     missing-fingerprint nudges. Warn palette regardless of the
    //     row's verdict; on a rejected row they sit alongside the
    //     error-coloured reject reasons so the user can tell the
    //     "you might want to" lines from the "this won't upload" lines.
    //   * `reason_style` — acceptance-band reject reasons. Always
    //     error-coloured; only present on rejected rows.
    let warning_style = "font-size: 11px; color: var(--warning); margin-top: 4px; padding: 3px 6px; background: var(--warning-bg); border-radius: 3px;";
    let reason_style = "font-size: 11px; color: var(--error); margin-top: 4px; padding: 3px 6px; background: var(--bg); border: 1px solid var(--error); border-radius: 3px;";

    rsx! {
        div {
            class: "staged-row fade-in",
            style: "{row_style}",

            // Filename + size, plus an inline REJECTED chip when applicable
            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                div {
                    style: "flex: 1; min-width: 0;",
                    div {
                        style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                        if is_rejected {
                            span {
                                style: "display: inline-block; font-size: 10px; font-weight: 700; letter-spacing: 0.05em; padding: 1px 5px; margin-right: 6px; border-radius: 3px; background: var(--error); color: white; vertical-align: 1px;",
                                "REJECTED"
                            }
                        }
                        "{task.filename}"
                    }
                }
                span { style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0; margin-left: 8px;", "{format_size(task.size)}" }
            }

            if let Some(details) = video_details.clone() {
                VideoInfoPopover { details }
            }

            // Reject reasons render first so they read as the headline
            // for a rejected row; advisory warnings follow underneath in
            // the warn palette.
            for reason in task.rejection_reasons.iter() {
                div {
                    style: "{reason_style}",
                    "{reason}"
                }
            }
            for warning in task.validation_warnings.iter() {
                div {
                    style: "{warning_style}",
                    "{warning}"
                }
            }

            // Action buttons
            div {
                style: "display: flex; justify-content: flex-end; align-items: center; gap: 6px; margin-top: 8px;",
                if show_already_ok_badge {
                    span {
                        style: "font-size: 11px; color: var(--text-secondary); padding: 2px 6px; border-radius: 3px; background: var(--bg-secondary); border: 1px solid var(--border);",
                        title: "Source already at or below transcode targets — no benefit to re-encoding.",
                        "Already matches targets"
                    }
                }
                if show_transcode_toggle {
                    button {
                        style: "{transcode_btn_style}",
                        onclick: move |_| on_transcode_click.call(transcode_id.clone()),
                        "{transcode_label}"
                    }
                }
                if let (true, Some(handler)) = (is_super_admin && is_rejected, on_force_upload) {
                    button {
                        class: "btn-warning-sm",
                        style: "height: 24px; padding: 0 8px; font-size: 11px; border-radius: 4px; cursor: pointer; background: var(--warning); color: white; border: 1px solid var(--warning);",
                        title: "Bypass dedup and quality checks for this file. Visible to super-admins only.",
                        onclick: move |_| handler.call(force_id.clone()),
                        "Force upload"
                    }
                }
                button {
                    class: "btn-danger-sm",
                    style: "{styles::BTN_DANGER_SM}",
                    onclick: move |_| on_remove.call(task_id.clone()),
                    "Remove"
                }
            }
        }
    }
}

#[component]
fn UploadTaskRow(
    task: UploadTask,
    transcode_progress: Signal<HashMap<String, f32>>,
    upload_progress: Signal<HashMap<String, (u64, u64)>>,
    on_retry: EventHandler<String>,
    on_remove: EventHandler<String>,
    on_pause: EventHandler<String>,
    on_resume: EventHandler<String>,
) -> Element {
    let app_state = use_context::<AppState>();

    // The bytes-actually-uploaded denominator: transcoded size when we have
    // it, otherwise the original. GCS only ever sees one of these on the wire.
    let upload_total = task.transcoded_size.unwrap_or(task.size);

    // Phase-aware progress reader. Read from the live signals only — never
    // from the cloned task's `bytes_uploaded`, which lags behind and snaps
    // back to 0 on render races.
    let (progress_pct, uploaded_bytes) = match task.state {
        UploadState::Completed => (100u32, upload_total),
        UploadState::Transcoding => {
            let pct = transcode_progress
                .read()
                .get(&task.id)
                .copied()
                .unwrap_or(0.0) as u32;
            (pct.min(100), 0u64)
        }
        UploadState::Uploading | UploadState::Verifying | UploadState::Paused => {
            match upload_progress.read().get(&task.id) {
                Some(&(uploaded, total)) if total > 0 => {
                    let pct = (uploaded as f64 / total as f64 * 100.0) as u32;
                    (pct.min(100), uploaded)
                }
                _ => (0, 0),
            }
        }
        _ => (0, 0),
    };

    let (status_color, status_bg) = match task.state {
        UploadState::Completed => ("var(--success)", "var(--success-bg)"),
        UploadState::Failed => ("var(--error)", "var(--error-bg)"),
        UploadState::Uploading => ("var(--info)", "var(--info-bg)"),
        UploadState::Paused => ("var(--warning)", "var(--warning-bg)"),
        _ => ("var(--text-muted)", "var(--bg-secondary)"),
    };

    let tenant_name = app_state.tenant_display_name(&task.tenant_id);
    let project_name = app_state.project_display_name(&task.tenant_id, &task.project_id);

    // Size column: "original" for non-transcoded tasks, "original → transcoded"
    // when transcode is enabled. The transcoded half is "…" until the engine
    // emits TranscodeCompleted.
    let size_line = if task.transcode {
        match task.transcoded_size {
            Some(tr) => format!("{} → {}", format_size(task.size), format_size(tr)),
            None => format!("{} → …", format_size(task.size)),
        }
    } else {
        format_size(task.size)
    };

    rsx! {
        div {
            class: "card-row",
            style: "padding: 10px 12px; border: 1px solid var(--border); border-radius: 6px; background: {status_bg}; transition: background 0.15s, border-color 0.15s, box-shadow 0.15s;",

            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                div {
                    style: "flex: 1; min-width: 0;",
                    span {
                        style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block;",
                        "{task.filename}"
                    }
                    span {
                        style: "font-size: 11px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block; margin-top: 2px;",
                        "{tenant_name} / {project_name}"
                    }
                }
                div {
                    style: "display: flex; align-items: center; gap: 6px; margin-left: 8px; flex-shrink: 0;",
                    span {
                        style: "font-size: 11px; color: {status_color}; font-weight: 600; text-transform: uppercase;",
                        "{task.state.as_str()}"
                    }
                    // Action buttons based on state
                    {
                        let id1 = task.id.clone();
                        let id2 = task.id.clone();
                        let small_btn = "height: 24px; padding: 0 8px; font-size: 11px; border-radius: 4px; cursor: pointer; transition: background 0.15s, transform 0.08s;";
                        match task.state {
                            UploadState::Uploading
                            | UploadState::Validating
                            | UploadState::Transcoding
                            | UploadState::Desensitizing
                            | UploadState::Creating
                            | UploadState::Verifying
                            | UploadState::Pending => rsx! {
                                button {
                                    class: "btn-outline",
                                    style: "{small_btn} background: transparent; color: var(--warning); border: 1px solid var(--warning);",
                                    onclick: move |_| on_pause.call(id1.clone()),
                                    "Pause"
                                }
                            },
                            UploadState::Paused => rsx! {
                                button {
                                    class: "btn-primary",
                                    style: "{small_btn} background: var(--btn-primary); color: white; border: none;",
                                    onclick: move |_| on_resume.call(id1.clone()),
                                    "Resume"
                                }
                                button {
                                    class: "btn-danger-sm",
                                    style: "{small_btn} background: transparent; color: var(--error); border: 1px solid var(--error);",
                                    onclick: move |_| on_remove.call(id2.clone()),
                                    "Remove"
                                }
                            },
                            UploadState::Failed => rsx! {
                                button {
                                    class: "btn-primary",
                                    style: "{small_btn} background: var(--btn-primary); color: white; border: none;",
                                    onclick: move |_| on_retry.call(id1.clone()),
                                    "Retry"
                                }
                                button {
                                    class: "btn-danger-sm",
                                    style: "{small_btn} background: transparent; color: var(--error); border: 1px solid var(--error);",
                                    onclick: move |_| on_remove.call(id2.clone()),
                                    "Remove"
                                }
                            },
                            UploadState::Completed => rsx! {
                                button {
                                    class: "btn-outline",
                                    style: "{small_btn} background: transparent; color: var(--text-muted); border: 1px solid var(--border);",
                                    onclick: move |_| on_remove.call(id1.clone()),
                                    "Clear"
                                }
                            },
                            UploadState::QualityChecking
                            | UploadState::Staged
                            | UploadState::Rejected
                            | UploadState::Hashing => rsx! {},
                        }
                    }
                }
            }

            div {
                style: "font-size: 12px; color: var(--text-muted); margin-top: 4px;",
                "{size_line}"
            }

            {
                let show_progress = task.state.is_active() || task.state == UploadState::Paused;
                let phase_label = phase_label(&task.state, progress_pct, uploaded_bytes, upload_total);
                let bar_wrapper_style = if show_progress {
                    "margin-top: 6px;"
                } else {
                    "display: none;"
                };
                let label_style = if show_progress {
                    "font-size: 11px; margin-top: 2px; color: var(--text-muted);"
                } else {
                    "display: none;"
                };
                let paused_class = if task.state == UploadState::Paused {
                    "progress-paused"
                } else {
                    ""
                };
                let value = progress_pct as f64;
                rsx! {
                    div {
                        style: "{bar_wrapper_style}",
                        Progress {
                            class: paused_class,
                            value,
                            max: 100.0,
                            "aria-label": "Upload progress",
                            ProgressIndicator {}
                        }
                    }
                    div {
                        style: "{label_style}",
                        "{phase_label}"
                    }
                }
            }

            if let Some(ref err) = task.error_message {
                div {
                    style: "font-size: 12px; color: var(--error); margin-top: 4px; padding: 6px 8px; background: var(--error-bg); border-radius: 4px;",
                    "{err}"
                }
            }

            for reason in task.rejection_reasons.iter() {
                div {
                    style: "font-size: 12px; color: var(--error); margin-top: 2px; padding: 4px 8px; background: var(--error-bg); border-radius: 4px;",
                    "{reason}"
                }
            }
            for warning in task.validation_warnings.iter() {
                div {
                    style: "font-size: 12px; color: var(--warning); margin-top: 2px; padding: 4px 8px; background: var(--warning-bg); border-radius: 4px;",
                    "{warning}"
                }
            }
        }
    }
}

/// Phase label rendered under the progress bar. Single bar, single label —
/// the state determines whether we're showing transcode %, upload bytes,
/// or a static stage name.
fn phase_label(state: &UploadState, pct: u32, uploaded: u64, total: u64) -> String {
    match state {
        UploadState::Transcoding => format!("Transcoding {pct}%"),
        UploadState::Uploading => format!(
            "Uploading {pct}% — {} / {}",
            format_size(uploaded),
            format_size(total)
        ),
        UploadState::Validating => "Validating...".to_string(),
        UploadState::Desensitizing => "Desensitizing...".to_string(),
        UploadState::Creating => "Creating...".to_string(),
        UploadState::Verifying => "Verifying...".to_string(),
        UploadState::Paused => "Paused".to_string(),
        UploadState::Pending => "Pending...".to_string(),
        UploadState::QualityChecking
        | UploadState::Hashing
        | UploadState::Staged
        | UploadState::Rejected
        | UploadState::Completed
        | UploadState::Failed => String::new(),
    }
}

// `handle_upload_event` and `update_task` moved to `upload_runtime.rs`
// alongside the resident event pump that calls them.

// The transcode dialog, its `ButtonGroup` facade, and related constants now
// live in `transcode_dialog.rs`.

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(secs: f64) -> String {
    let secs = secs.max(0.0);
    let total = secs.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
