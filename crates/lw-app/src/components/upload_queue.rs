use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload::UploadEvent;
use std::collections::HashMap;
use std::path::PathBuf;

#[component]
pub fn UploadQueue() -> Element {
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    // Load history from SQLite on mount
    let app_state_load = app_state.clone();
    let db_for_load = services.db.clone();
    use_future(move || {
        let db = db_for_load.clone();
        let mut app_state = app_state_load.clone();
        async move {
            // Reset stale in-progress uploads to FAILED
            match db.reset_stale_uploads().await {
                Ok(n) if n > 0 => tracing::info!("Reset {n} stale uploads to FAILED"),
                Err(e) => tracing::warn!("Failed to reset stale uploads: {e}"),
                _ => {}
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

    // Progress signals — separate from task state to avoid re-render oscillation
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

            // Transcode config dialog overlay
            if transcode_dialog_task.read().is_some() {
                {
                    let mut app_state_dialog = app_state.clone();
                    rsx! {
                        TranscodeDialog {
                            task_id: transcode_dialog_task.read().clone().unwrap_or_default(),
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
    on_remove: EventHandler<String>,
    on_transcode_click: EventHandler<String>,
) -> Element {
    let task_id = task.id.clone();
    let transcode_id = task.id.clone();
    let is_video = task.mime_type.starts_with("video/");
    let transcode_on = task.transcode;

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
                style: "display: flex; justify-content: flex-end; gap: 6px; margin-top: 8px;",
                if is_video {
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
    // Read progress from dedicated signals (not task.bytes_uploaded) to avoid render oscillation
    let (progress, bytes_uploaded) = if task.state == UploadState::Transcoding {
        let pct = transcode_progress
            .read()
            .get(&task.id)
            .copied()
            .unwrap_or(0.0) as u32;
        (pct, 0u64)
    } else if let Some(&(uploaded, total)) = upload_progress.read().get(&task.id) {
        let pct = if total > 0 {
            (uploaded as f64 / total as f64 * 100.0) as u32
        } else {
            0
        };
        (pct, uploaded)
    } else if task.size > 0 {
        let pct = (task.bytes_uploaded as f64 / task.size as f64 * 100.0) as u32;
        (pct, task.bytes_uploaded)
    } else {
        (0, 0)
    };

    let progress_color = if task.state == UploadState::Paused {
        "var(--warning)"
    } else {
        "var(--info)"
    };
    let state_label = match task.state {
        UploadState::Validating => "Validating...",
        UploadState::Transcoding => "Transcoding...",
        UploadState::Desensitizing => "Desensitizing...",
        UploadState::Creating => "Creating...",
        UploadState::Uploading => "Uploading",
        UploadState::Verifying => "Verifying...",
        UploadState::Paused => "Paused",
        UploadState::Pending => "Pending...",
        UploadState::Staged => "",
        UploadState::Completed => "",
        UploadState::Failed => "",
    };

    let (status_color, status_bg) = match task.state {
        UploadState::Completed => ("var(--success)", "var(--success-bg)"),
        UploadState::Failed => ("var(--error)", "var(--error-bg)"),
        UploadState::Uploading => ("var(--info)", "var(--info-bg)"),
        UploadState::Paused => ("var(--warning)", "var(--warning-bg)"),
        _ => ("var(--text-muted)", "var(--bg-secondary)"),
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
                        let _id3 = task.id.clone();
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
                style: "font-size: 12px; color: var(--text-muted); margin-top: 2px;",
                "{format_size(task.size)}"
            }

            {
                let show_progress = task.state.is_active() || task.state == UploadState::Paused;
                let bar_visibility = if show_progress { "margin-top: 6px; height: 4px;" } else { "margin-top: 0; height: 0;" };
                let label_visibility = if show_progress { "font-size: 11px; margin-top: 2px;" } else { "font-size: 0; margin-top: 0; height: 0; overflow: hidden;" };
                rsx! {
                    div {
                        style: "{bar_visibility} background: var(--border); border-radius: 2px; overflow: hidden; transition: height 0.2s ease;",
                        div {
                            style: "height: 100%; min-height: 4px; width: {progress}%; background: {progress_color}; transition: width 0.3s ease;",
                        }
                    }
                    div {
                        style: "{label_visibility} color: var(--text-muted); transition: height 0.2s ease, font-size 0.2s ease;",
                        if task.state == UploadState::Transcoding {
                            "{state_label} {progress}%"
                        } else if task.state == UploadState::Uploading {
                            "{state_label} {progress}% — {format_size(bytes_uploaded)} / {format_size(task.size)}"
                        } else if show_progress {
                            "{state_label}"
                        }
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
            upload_progress
                .write()
                .insert(task_id, (bytes_uploaded, total_bytes));
        }
        UploadEvent::ValidationWarnings { task_id, warnings } => {
            update_task(app_state, &task_id, |t| t.validation_warnings = warnings);
        }
        UploadEvent::TranscodeProgress { task_id, percent } => {
            transcode_progress.write().insert(task_id, percent);
        }
        UploadEvent::DuplicateDetected { task_id, .. } => {
            update_task(app_state, &task_id, |t| {
                t.state = UploadState::Failed;
                t.error_message = Some("Duplicate file detected".to_string());
            });
        }
        UploadEvent::Completed { task_id } => {
            update_task(app_state, &task_id, |t| t.state = UploadState::Completed);
        }
        UploadEvent::Failed { task_id, error } => {
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

// ── Transcode config dialog ──────────────────────────────────────────

const PRESETS: &[&str] = &["fast", "medium", "slow"];
const RESOLUTIONS: &[(u32, &str)] = &[(720, "720p"), (1080, "1080p")];
const AUDIO_BITRATES: &[u32] = &[128, 192];
const FPS_OPTIONS: &[(u32, &str)] = &[(24, "24fps"), (30, "30fps"), (60, "60fps")];

#[component]
fn TranscodeDialog(task_id: String, on_close: EventHandler<bool>) -> Element {
    let app_state = use_context::<AppState>();
    let mut config = use_signal(|| {
        lw_core::config::AppConfig::load()
            .map(|c| c.transcode)
            .unwrap_or_default()
    });

    // Find the task to show estimated output size
    let task_info = app_state
        .upload_tasks
        .read()
        .iter()
        .find(|t| t.id == task_id)
        .and_then(|t| t.video_info.clone());

    let estimated = task_info
        .as_ref()
        .map(|info| lw_core::transcode::estimate_transcoded_size(info, &config.read()));

    let on_ok = move |_| {
        // Save config to disk
        if let Ok(mut app_config) = lw_core::config::AppConfig::load() {
            app_config.transcode = config.read().clone();
            if let Err(e) = app_config.save() {
                tracing::error!("Failed to save transcode config: {e}");
            }
        }
        on_close.call(true);
    };

    let label_style = "font-size: 12px; font-weight: 500; color: var(--text); margin-bottom: 4px; display: block;";
    let src_height = task_info.as_ref().map(|i| i.height).unwrap_or(u32::MAX);
    let src_fps = task_info.as_ref().map(|i| i.fps as u32).unwrap_or(u32::MAX);
    let current_bitrate = config.read().max_bitrate_mbps;

    rsx! {
        // Overlay backdrop
        div {
            style: "position: fixed; inset: 0; background: rgba(0,0,0,0.4); z-index: 100; display: flex; align-items: center; justify-content: center;",
            onclick: move |_| on_close.call(false),

            // Dialog
            div {
                style: "background: var(--bg); border: 1px solid var(--border); border-radius: 8px; padding: 16px; width: 320px; max-height: 80vh; overflow-y: auto; box-shadow: 0 8px 32px rgba(0,0,0,0.2);",
                onclick: move |e| e.stop_propagation(),

                h3 {
                    style: "margin: 0 0 12px; font-size: 15px; font-weight: 600; color: var(--text);",
                    "Transcode Settings"
                }

                // Estimated output
                if let Some(est) = estimated {
                    div {
                        style: "font-size: 12px; color: var(--text-secondary); margin-bottom: 12px; padding: 6px 8px; background: var(--bg-secondary); border-radius: 4px;",
                        "Estimated output: ~{format_size(est)}"
                    }
                }

                // Preset — button group
                div {
                    style: "margin-bottom: 12px;",
                    label { style: label_style, "Speed" }
                    ButtonGroup {
                        options: PRESETS.iter().map(|p| (p.to_string(), p.to_string(), true)).collect(),
                        selected: config.read().preset.clone(),
                        on_select: move |v: String| config.write().preset = v,
                    }
                }

                // Resolution — button group (disable above source)
                div {
                    style: "margin-bottom: 12px;",
                    label { style: label_style, "Resolution" }
                    ButtonGroup {
                        options: RESOLUTIONS
                            .iter()
                            .map(|(h, l)| (h.to_string(), l.to_string(), *h <= src_height))
                            .collect(),
                        selected: config.read().max_height.to_string(),
                        on_select: move |v: String| {
                            if let Ok(h) = v.parse::<u32>() {
                                config.write().max_height = h;
                            }
                        },
                    }
                }

                // FPS — button group (disable above source)
                div {
                    style: "margin-bottom: 12px;",
                    label { style: label_style, "Frame Rate" }
                    {
                        let mut fps_opts: Vec<(String, String, bool)> =
                            vec![("0".to_string(), "Original".to_string(), true)];
                        fps_opts.extend(
                            FPS_OPTIONS
                                .iter()
                                .map(|(f, l)| (f.to_string(), l.to_string(), *f <= src_fps)),
                        );
                        rsx! {
                            ButtonGroup {
                                options: fps_opts,
                                selected: config.read().target_fps.to_string(),
                                on_select: move |v: String| {
                                    if let Ok(f) = v.parse::<u32>() {
                                        config.write().target_fps = f;
                                    }
                                },
                            }
                        }
                    }
                }

                // Max bitrate — range slider (5–20 Mbps, recommend 10)
                {
                    let (bitrate_color, bitrate_hint) = if (7..=15).contains(&current_bitrate) {
                        ("var(--success)", "Recommended")
                    } else if current_bitrate <= 6 {
                        ("var(--error)", "Low quality")
                    } else {
                        ("var(--warning)", "Large file size")
                    };
                    rsx! {
                        div {
                            style: "margin-bottom: 12px;",
                            label {
                                style: label_style,
                                "Max Bitrate: "
                                span { style: "color: {bitrate_color};", "{current_bitrate} Mbps" }
                                span { style: "font-size: 10px; color: {bitrate_color}; margin-left: 6px; font-weight: 400;", "({bitrate_hint})" }
                            }
                            input {
                                r#type: "range",
                                min: "5",
                                max: "20",
                                value: "{current_bitrate}",
                                onchange: move |evt: Event<FormData>| {
                                    if let Ok(v) = evt.value().parse::<u32>() {
                                        config.write().max_bitrate_mbps = v;
                                    }
                                },
                                style: "width: 100%; accent-color: {bitrate_color};",
                            }
                            div {
                                style: "display: flex; justify-content: space-between; font-size: 10px; color: var(--text-muted);",
                                span { "5 Mbps" }
                                span { "20 Mbps" }
                            }
                        }
                    }
                }

                // Audio bitrate — button group
                div {
                    style: "margin-bottom: 14px;",
                    label { style: label_style, "Audio Bitrate" }
                    ButtonGroup {
                        options: AUDIO_BITRATES
                            .iter()
                            .map(|r| (r.to_string(), format!("{r}k"), true))
                            .collect(),
                        selected: config.read().audio_bitrate_kbps.to_string(),
                        on_select: move |v: String| {
                            if let Ok(r) = v.parse::<u32>() {
                                config.write().audio_bitrate_kbps = r;
                            }
                        },
                    }
                }

                // Actions
                div {
                    style: "display: flex; gap: 8px;",
                    button {
                        style: "flex: 1; padding: 7px 14px; border-radius: 6px; border: none; background: var(--btn-primary); color: white; cursor: pointer; font-weight: 500; font-size: 13px;",
                        onclick: on_ok,
                        "Enable Transcode"
                    }
                    button {
                        style: "padding: 7px 14px; border-radius: 6px; border: 1px solid var(--border); background: transparent; color: var(--text-secondary); cursor: pointer; font-size: 13px;",
                        onclick: move |_| on_close.call(false),
                        "Cancel"
                    }
                }
            }
        }
    }
}

/// Segmented button group. Each option is (value, label, enabled).
#[component]
fn ButtonGroup(
    options: Vec<(String, String, bool)>,
    selected: String,
    on_select: EventHandler<String>,
) -> Element {
    rsx! {
        div {
            style: "display: flex; border: 1px solid var(--border); border-radius: 6px; overflow: hidden;",
            for (value, label, enabled) in options.iter() {
                {
                    let is_selected = *value == selected;
                    let is_enabled = *enabled;
                    let bg = if is_selected {
                        "var(--btn-primary)"
                    } else {
                        "transparent"
                    };
                    let color = if is_selected {
                        "white"
                    } else if is_enabled {
                        "var(--text-secondary)"
                    } else {
                        "var(--text-muted)"
                    };
                    let opacity = if is_enabled { "1" } else { "0.5" };
                    let cursor = if is_enabled { "pointer" } else { "not-allowed" };
                    let val = value.clone();
                    rsx! {
                        button {
                            style: "flex: 1; padding: 5px 8px; font-size: 12px; border: none; border-right: 1px solid var(--border); background: {bg}; color: {color}; opacity: {opacity}; cursor: {cursor}; transition: background 0.15s;",
                            disabled: !is_enabled,
                            onclick: move |_| on_select.call(val.clone()),
                            "{label}"
                        }
                    }
                }
            }
        }
    }
}

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
