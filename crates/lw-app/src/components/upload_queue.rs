use crate::components::progress::{Progress, ProgressIndicator};
use crate::components::transcode_dialog::TranscodeDialog;
use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use lw_core::config::TranscodeConfig;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload::UploadEvent;
use lw_core::video;
use std::collections::HashMap;
use std::path::PathBuf;

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

    let tasks = app_state.upload_tasks.read();
    let has_context =
        app_state.selected_tenant.read().is_some() && app_state.selected_project.read().is_some();

    let staged_count = tasks
        .iter()
        .filter(|t| t.state == UploadState::Staged)
        .count();
    let _active_count = tasks.iter().filter(|t| t.state.is_active()).count();

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

        spawn(async move {
            let files = rfd::AsyncFileDialog::new()
                .set_title("Select files to upload")
                .pick_files()
                .await;
            let Some(files) = files else { return };
            for file in files {
                let path = PathBuf::from(file.path());
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    tracing::error!("Failed to stage file: {e}");
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
                        tracing::error!("Retry failed for {}: {e}", task.filename);
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
                        tracing::error!("Resume failed for {}: {e}", task.filename);
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
        if !has_context {
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
        let files = evt.files();
        spawn(async move {
            for file in files {
                let path = file.path();
                if path.as_os_str().is_empty() {
                    continue;
                }
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    tracing::error!("Failed to stage dropped file: {e}");
                }
            }
        });
    };

    let drop_border = if *is_dragging.read() && has_context {
        "2px dashed var(--border-focus)"
    } else {
        "2px dashed transparent"
    };

    // Split tasks into sections
    let staged: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Staged)
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
            style: "padding: 16px; border: {drop_border}; border-radius: 8px; min-height: 300px; transition: border 0.2s;",
            ondragover: move |evt| { evt.prevent_default(); is_dragging.set(true); },
            ondragleave: move |_| is_dragging.set(false),
            ondrop: on_drop,

            // Header
            div {
                style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px;",
                h2 { style: "margin: 0; font-size: 16px;", "Upload Queue" }
                div {
                    style: "display: flex; gap: 8px; align-items: center;",
                    if has_context {
                        button {
                            class: "btn-primary",
                            style: "{styles::BTN_PRIMARY}",
                            onclick: on_add_files,
                            "Add Files"
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
                        button { style: "{styles::BTN_DISABLED}", disabled: true, "Add Files" }
                    }
                }
            }

            if !has_context {
                div {
                    style: "text-align: center; padding: 48px 16px; color: var(--text-muted); font-size: 14px;",
                    "Select an organization and project to start uploading"
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
                                transcode_config: services.config.transcode.clone(),
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

                // Empty state
                if staged.is_empty() && transcoding.is_empty() && active.is_empty() && history.is_empty() {
                    div {
                        style: "text-align: center; padding: 48px 16px; color: var(--text-muted);",
                        p { style: "font-size: 14px; margin: 0 0 4px;", "No files in queue" }
                        p { style: "font-size: 13px; margin: 0;", "Drop files here or click \"Add Files\"" }
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
    let show_transcode_toggle = is_video && transcode_useful;
    let show_already_ok_badge = is_video && !transcode_useful;

    // Build video info summary line
    let video_summary = task.video_info.as_ref().map(|info| {
        let codec = info.codec.to_uppercase();
        let res = format!("{}x{}", info.width, info.height);
        let fps = format!("{:.0}fps", info.fps);
        let bitrate = if info.bitrate_kbps >= 1000 {
            format!("{:.0}Mbps", info.bitrate_kbps as f64 / 1000.0)
        } else {
            format!("{}kbps", info.bitrate_kbps)
        };
        format!("{codec} · {res} · {fps} · {bitrate}")
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

    rsx! {
        div {
            class: "staged-row fade-in",
            style: "padding: 10px 12px; border: 1px solid var(--staged-border); border-radius: 6px; background: var(--staged-bg); transition: background 0.15s, border-color 0.15s;",

            // Filename + size
            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                div {
                    style: "flex: 1; min-width: 0;",
                    div { style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;", "{task.filename}" }
                }
                span { style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0; margin-left: 8px;", "{format_size(task.size)}" }
            }

            // Video info line
            if let Some(summary) = &video_summary {
                div {
                    style: "font-size: 11px; color: var(--text-secondary); margin-top: 4px;",
                    "{summary}"
                }
            }

            // Validation warnings
            for warning in task.validation_warnings.iter() {
                div {
                    style: "font-size: 11px; color: var(--warning); margin-top: 4px; padding: 3px 6px; background: var(--warning-bg); border-radius: 3px;",
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
                            UploadState::Staged => rsx! {},
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
        UploadState::Staged | UploadState::Completed | UploadState::Failed => String::new(),
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
