use crate::components::progress::{Progress, ProgressIndicator};
use crate::components::transcode_dialog::TranscodeDialog;
use crate::state::{AppState, CoreServices, ToastKind};
use crate::styles;
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use lw_core::config::TranscodeConfig;
use lw_core::error::UploadError;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload::{self, UploadEvent};
use lw_core::video;
use lw_core::video_rules::DeviceEncoderSignature;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Formats a staging error into a user-facing toast string. Expected
/// rejections (the user picked a file we can't accept) get a "Cannot
/// upload" prefix and the typed error's `Display` as the reason.
/// Unexpected failures (network, IO, API, DB) get a "Failed to add"
/// prefix — they look the same to the user, but the prefix matches the
/// log-level distinction in `UploadError::log`.
///
/// Listed exhaustively rather than via a catch-all so that adding a new
/// `UploadError` variant forces a deliberate routing decision here.
fn stage_error_toast(path: &Path, err: &UploadError) -> String {
    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    match err {
        UploadError::VideoUnplayable { .. }
        | UploadError::Duplicate { .. }
        | UploadError::FileTooLarge { .. }
        | UploadError::FileNotFound(_)
        | UploadError::Cancelled => format!("Cannot upload \"{filename}\": {err}"),
        UploadError::Api { .. }
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

    // Load history from SQLite on mount, then resume in-flight work.
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

    // Progress signals — separate from task state to avoid re-render oscillation.
    // `upload_progress` is kept monotonic per task: once we've seen a higher
    // bytes-uploaded value for a task id, we never regress to a lower one in
    // the UI. This prevents two legitimate-but-confusing zero-dips: (a) the
    // GCS resumable-session retry path at upload.rs re-initiates a session
    // and emits a fresh Progress(0, total), and (b) a render that lands on a
    // task before the first Progress event arrives.
    let mut transcode_progress: Signal<HashMap<String, f32>> = use_signal(HashMap::new);
    let mut upload_progress: Signal<HashMap<String, (u64, u64)>> = use_signal(HashMap::new);

    // Poll upload events
    let app_state_events = app_state.clone();
    use_future(move || {
        let event_rx = services.event_rx.clone();
        let mut app_state = app_state_events.clone();
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
                    event,
                );
            }
        }
    });

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

    // Device-encoder signature list lives on the network-loaded video
    // rules document, already wrapped in Arc inside ProvenanceRules so
    // each StagedRow prop clone is a refcount bump rather than a fresh
    // Vec allocation — the queue can carry dozens of rows and re-renders
    // on every keystroke that changes a sibling signal.
    let device_encoder_signatures: Arc<Vec<DeviceEncoderSignature>> = Arc::clone(
        &services
            .upload_engine
            .video_rules()
            .provenance
            .device_encoder_signatures,
    );

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
            let files = rfd::AsyncFileDialog::new()
                .set_title("Select files to upload")
                .pick_files()
                .await;
            let Some(files) = files else { return };
            for file in files {
                let path = PathBuf::from(file.path());
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    e.log("Stage file");
                    app_state_for_toast.show_toast(stage_error_toast(&path, &e), ToastKind::Error);
                }
            }
        });
    };

    // Confirm upload (step 2)
    let engine_for_confirm = services.upload_engine.clone();
    let app_state_for_confirm = app_state.clone();
    let mut app_state_confirm = app_state.clone();
    let on_confirm = move |_| {
        let engine = engine_for_confirm.clone();
        // Collect transcode-opted task IDs from UI signal
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
                    tracing::info!("Confirmed {} files for upload", ids.len());
                    let mut tasks = app_state_confirm.upload_tasks.write();
                    for task in tasks.iter_mut() {
                        if ids.contains(&task.id) {
                            task.state = UploadState::Pending;
                        }
                    }
                }
                Err(e) => tracing::error!("Failed to confirm uploads: {e}"),
            }
        });
    };

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
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    e.log("Stage dropped file");
                    app_state_for_toast.show_toast(stage_error_toast(&path, &e), ToastKind::Error);
                }
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
    let active: Vec<_> = tasks
        .iter()
        .filter(|t| t.state.is_active() && t.state != UploadState::Transcoding)
        .cloned()
        .collect();
    let history: Vec<_> = tasks
        .iter()
        .filter(|t| matches!(t.state, UploadState::Completed | UploadState::Failed))
        .cloned()
        .collect();

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
                                rsx! {
                                    button {
                                        class: "btn-success",
                                        style: "{styles::BTN_SUCCESS}",
                                        onclick: on_confirm,
                                        "{label}"
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
                if !staged.is_empty() {
                    SectionHeader { title: "Ready to Upload", count: staged.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                        for task in staged.iter() {
                            StagedRow {
                                key: "{task.id}",
                                task: task.clone(),
                                transcode_config: app_state.config.read().transcode.clone(),
                                device_encoder_signatures: device_encoder_signatures.clone(),
                                on_remove: on_remove.clone(),
                                on_transcode_click,
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
                                device_encoder_signatures: device_encoder_signatures.clone(),
                                on_remove: on_remove.clone(),
                                on_transcode_click,
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
                if staged.is_empty() && rejected.is_empty() && transcoding.is_empty() && active.is_empty() && history.is_empty() {
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

#[component]
fn StagedRow(
    task: UploadTask,
    transcode_config: TranscodeConfig,
    device_encoder_signatures: Arc<Vec<DeviceEncoderSignature>>,
    on_remove: EventHandler<String>,
    on_transcode_click: EventHandler<String>,
) -> Element {
    let task_id = task.id.clone();
    let transcode_id = task.id.clone();
    let is_video = task.mime_type.starts_with("video/");
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

    // Build video info summary line and the matching popover groups.
    // The summary line is the affordance the user hovers; the popover
    // shows three groups stacked: the structural numbers (codec, res, fps,
    // bitrate, audio, duration, container), the device-info group with
    // a Telemetry row, and a flat dump of every readable container /
    // stream tag for transparency.
    let video_details = task.video_info.as_ref().map(|info| {
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
            video::device_info_rows(info, device_encoder_signatures.as_slice())
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

        (summary, structural, device, raw)
    });

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
    let warning_style = if is_rejected {
        "font-size: 11px; color: var(--error); margin-top: 4px; padding: 3px 6px; background: var(--bg); border: 1px solid var(--error); border-radius: 3px;"
    } else {
        "font-size: 11px; color: var(--warning); margin-top: 4px; padding: 3px 6px; background: var(--warning-bg); border-radius: 3px;"
    };

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

            // Video info line + hover popover. Two-column layout when
            // raw tags are present: left column stacks the structural
            // numbers and the device-info group; right column is the raw
            // container / stream dump. When raw is empty, the panel
            // collapses to a single column.
            if let Some((summary, structural, device, raw)) = &video_details {
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

            // Validation warnings (rendered red when the row is REJECTED so
            // the rejection reasons read as the cause, not a side note).
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
                            UploadState::Staged | UploadState::Rejected => rsx! {},
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
        UploadState::Staged
        | UploadState::Rejected
        | UploadState::Completed
        | UploadState::Failed => String::new(),
    }
}

fn handle_upload_event(
    app_state: &mut AppState,
    transcode_progress: &mut Signal<HashMap<String, f32>>,
    upload_progress: &mut Signal<HashMap<String, (u64, u64)>>,
    event: UploadEvent,
) {
    match event {
        UploadEvent::TaskAdded(task) => {
            app_state.upload_tasks.write().push(*task);
        }
        UploadEvent::StateChanged { task_id, state } => {
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
        UploadEvent::ValidationWarnings { task_id, warnings } => {
            update_task(app_state, &task_id, |t| t.validation_warnings = warnings);
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
            update_task(app_state, &task_id, |t| {
                t.state = UploadState::Failed;
                t.error_message = Some("Duplicate file detected".to_string());
            });
        }
        UploadEvent::Completed { task_id } => {
            upload_progress.write().remove(&task_id);
            transcode_progress.write().remove(&task_id);
            update_task(app_state, &task_id, |t| t.state = UploadState::Completed);
        }
        UploadEvent::Failed { task_id, error } => {
            upload_progress.write().remove(&task_id);
            transcode_progress.write().remove(&task_id);
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
