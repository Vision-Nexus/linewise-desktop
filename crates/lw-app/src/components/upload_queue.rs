use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::html::HasFileData;
use dioxus::prelude::*;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload::UploadEvent;
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
                handle_upload_event(&mut app_state, event);
            }
        }
    });

    let tasks = app_state.upload_tasks.read();
    let has_context = app_state.selected_tenant.read().is_some()
        && app_state.selected_project.read().is_some();

    let staged_count = tasks.iter().filter(|t| t.state == UploadState::Staged).count();
    let _active_count = tasks.iter().filter(|t| t.state.is_active()).count();

    // Stage files (step 1)
    let engine_for_add = services.upload_engine.clone();
    let engine_for_drop = services.upload_engine.clone();

    let app_state_add = app_state.clone();
    let on_add_files = move |_| {
        let engine = engine_for_add.clone();
        let tenant_id = app_state_add.selected_tenant.read().as_ref().map(|t| t.id.clone()).unwrap_or_default();
        let project_id = app_state_add.selected_project.read().as_ref().map(|p| p.id.clone()).unwrap_or_default();

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
    let mut app_state_confirm = app_state.clone();
    let on_confirm = move |_| {
        let engine = engine_for_confirm.clone();
        spawn(async move {
            match engine.confirm_staged().await {
                Ok(ids) => {
                    tracing::info!("Confirmed {} files for upload", ids.len());
                    // Update staged → pending in UI
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
            app_state_remove.upload_tasks.write().retain(|t| t.id != task_id);
        });
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

    // Remove from history (delete from DB + UI)
    let mut app_state_clear = app_state.clone();
    let db_for_clear = services.db.clone();
    let on_clear = move |task_id: String| {
        let db = db_for_clear.clone();
        spawn(async move {
            let _ = db.delete_upload_task(&task_id).await;
            app_state_clear.upload_tasks.write().retain(|t| t.id != task_id);
        });
    };

    // DnD
    let mut is_dragging = use_signal(|| false);
    let app_state_drop = app_state.clone();
    let on_drop = move |evt: DragEvent| {
        is_dragging.set(false);
        if !has_context { return; }
        let engine = engine_for_drop.clone();
        let tenant_id = app_state_drop.selected_tenant.read().as_ref().map(|t| t.id.clone()).unwrap_or_default();
        let project_id = app_state_drop.selected_project.read().as_ref().map(|p| p.id.clone()).unwrap_or_default();
        let files = evt.files();
        spawn(async move {
            for file in files {
                let path = file.path();
                if path.as_os_str().is_empty() { continue; }
                if let Err(e) = engine.stage_file(&path, &tenant_id, &project_id).await {
                    tracing::error!("Failed to stage dropped file: {e}");
                }
            }
        });
    };

    let drop_border = if *is_dragging.read() && has_context { "2px dashed var(--border-focus)" } else { "2px dashed transparent" };

    // Split tasks into sections
    let staged: Vec<_> = tasks.iter().filter(|t| t.state == UploadState::Staged).cloned().collect();
    let active: Vec<_> = tasks.iter().filter(|t| t.state.is_active()).cloned().collect();
    let history: Vec<_> = tasks.iter().filter(|t| matches!(t.state, UploadState::Completed | UploadState::Failed)).cloned().collect();

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
                                on_retry: on_retry.clone(),
                                on_remove: on_clear.clone(),
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
                                on_retry: on_retry.clone(),
                                on_remove: on_clear.clone(),
                            }
                        }
                    }
                }

                // Empty state
                if staged.is_empty() && active.is_empty() && history.is_empty() {
                    div {
                        style: "text-align: center; padding: 48px 16px; color: var(--text-muted);",
                        p { style: "font-size: 14px; margin: 0 0 4px;", "No files in queue" }
                        p { style: "font-size: 13px; margin: 0;", "Drop files here or click \"Add Files\"" }
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
fn StagedRow(task: UploadTask, on_remove: EventHandler<String>) -> Element {
    let task_id = task.id.clone();
    rsx! {
        div {
            class: "staged-row fade-in",
            style: "display: flex; align-items: center; justify-content: space-between; padding: 10px 12px; border: 1px solid var(--staged-border); border-radius: 6px; background: var(--staged-bg); transition: background 0.15s, border-color 0.15s;",
            div {
                style: "flex: 1; min-width: 0;",
                div { style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;", "{task.filename}" }
                div { style: "font-size: 12px; color: var(--text-muted); margin-top: 2px;", "{format_size(task.size)}" }
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

#[component]
fn UploadTaskRow(
    task: UploadTask,
    on_retry: EventHandler<String>,
    on_remove: EventHandler<String>,
) -> Element {
    let progress = if task.size > 0 {
        (task.bytes_uploaded as f64 / task.size as f64 * 100.0) as u32
    } else {
        0
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
                        let task_id = task.id.clone();
                        let task_id2 = task.id.clone();
                        match task.state {
                            UploadState::Failed => rsx! {
                                button {
                                    class: "btn-primary",
                                    style: "height: 24px; padding: 0 8px; font-size: 11px; border-radius: 4px; background: var(--btn-primary); color: white; border: none; cursor: pointer; transition: background 0.15s, transform 0.08s;",
                                    onclick: move |_| on_retry.call(task_id.clone()),
                                    "Retry"
                                }
                                button {
                                    class: "btn-danger-sm",
                                    style: "height: 24px; padding: 0 8px; font-size: 11px; border-radius: 4px; background: transparent; color: var(--error); border: 1px solid var(--error); cursor: pointer; transition: background 0.15s, transform 0.08s;",
                                    onclick: move |_| on_remove.call(task_id2.clone()),
                                    "Remove"
                                }
                            },
                            UploadState::Completed => rsx! {
                                button {
                                    class: "btn-danger-sm",
                                    style: "height: 24px; padding: 0 8px; font-size: 11px; border-radius: 4px; background: transparent; color: var(--text-muted); border: 1px solid var(--border); cursor: pointer; transition: background 0.15s, transform 0.08s;",
                                    onclick: move |_| on_remove.call(task_id.clone()),
                                    "Clear"
                                }
                            },
                            _ => rsx! {}
                        }
                    }
                }
            }

            div {
                style: "font-size: 12px; color: var(--text-muted); margin-top: 2px;",
                "{format_size(task.size)}"
            }

            if task.state == UploadState::Uploading {
                div {
                    style: "margin-top: 6px; height: 4px; background: var(--border); border-radius: 2px; overflow: hidden;",
                    div {
                        style: "height: 100%; width: {progress}%; background: var(--info); transition: width 0.3s ease;",
                    }
                }
                div {
                    style: "font-size: 11px; color: var(--text-muted); margin-top: 2px;",
                    "{progress}% — {format_size(task.bytes_uploaded)} / {format_size(task.size)}"
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

fn handle_upload_event(app_state: &mut AppState, event: UploadEvent) {
    match event {
        UploadEvent::TaskAdded(task) => {
            app_state.upload_tasks.write().push(*task);
        }
        UploadEvent::StateChanged { task_id, state } => {
            update_task(app_state, &task_id, |t| t.state = state);
        }
        UploadEvent::Progress { task_id, bytes_uploaded, .. } => {
            update_task(app_state, &task_id, |t| t.bytes_uploaded = bytes_uploaded);
        }
        UploadEvent::ValidationWarnings { task_id, warnings } => {
            update_task(app_state, &task_id, |t| t.validation_warnings = warnings);
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
