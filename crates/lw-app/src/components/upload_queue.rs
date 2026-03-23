use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;
use lw_core::models::{UploadState, UploadTask};
use lw_core::upload::UploadEvent;
use std::path::PathBuf;

#[component]
pub fn UploadQueue() -> Element {
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    // Poll upload events and update task list
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

    let app_state_upload = app_state.clone();
    let on_add_files = move |_| {
        let engine = services.upload_engine.clone();
        let tenant_id = app_state_upload
            .selected_tenant
            .read()
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        let project_id = app_state_upload
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
                match engine.queue_file(&path, &tenant_id, &project_id).await {
                    Ok(task) => {
                        tracing::info!("Queued: {}", task.filename);
                        // Start processing immediately
                        let eng = engine.clone();
                        let mut task = task;
                        tokio::spawn(async move {
                            if let Err(e) = eng.process_task(&mut task).await {
                                tracing::error!("Upload failed for {}: {e}", task.filename);
                            }
                        });
                    }
                    Err(e) => tracing::error!("Failed to queue file: {e}"),
                }
            }
        });
    };

    rsx! {
        div {
            style: "padding: 16px;",

            {
                let btn_bg = if has_context { "#2563eb" } else { "#9ca3af" };
                let btn_cursor = if has_context { "pointer" } else { "not-allowed" };
                rsx! {
                    div {
                        style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px;",
                        h2 { style: "margin: 0; font-size: 16px;", "Upload Queue" }
                        button {
                            style: "padding: 8px 16px; background: {btn_bg}; color: white; border: none; border-radius: 6px; cursor: {btn_cursor}; font-size: 13px; font-weight: 500;",
                            disabled: !has_context,
                            onclick: on_add_files,
                            "Add Files"
                        }
                    }
                }
            }

            if !has_context {
                div {
                    style: "text-align: center; padding: 32px; color: #9ca3af; font-size: 14px;",
                    "Select an organization and project to start uploading"
                }
            } else if tasks.is_empty() {
                div {
                    style: "text-align: center; padding: 48px; color: #9ca3af;",
                    p { style: "font-size: 14px;", "No files in queue" }
                    p { style: "font-size: 13px;", "Click \"Add Files\" to start uploading" }
                }
            } else {
                div { style: "display: flex; flex-direction: column; gap: 8px;",
                    for task in tasks.iter() {
                        UploadTaskRow {
                            key: "{task.id}",
                            filename: task.filename.clone(),
                            size: task.size,
                            bytes_uploaded: task.bytes_uploaded,
                            state: task.state.as_str().to_string(),
                            error_message: task.error_message.clone(),
                            warnings: task.validation_warnings.clone(),
                        }
                    }
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
        UploadEvent::Progress {
            task_id,
            bytes_uploaded,
            ..
        } => {
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

#[component]
fn UploadTaskRow(
    filename: String,
    size: u64,
    bytes_uploaded: u64,
    state: String,
    error_message: Option<String>,
    warnings: Vec<String>,
) -> Element {
    let progress = if size > 0 {
        (bytes_uploaded as f64 / size as f64 * 100.0) as u32
    } else {
        0
    };

    let status_color = match state.as_str() {
        "COMPLETED" => "#22c55e",
        "FAILED" => "#ef4444",
        "UPLOADING" => "#3b82f6",
        "PAUSED" => "#f59e0b",
        _ => "#6b7280",
    };

    rsx! {
        div {
            style: "padding: 12px; border: 1px solid #e5e7eb; border-radius: 8px; background: white;",

            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                span { style: "font-weight: 500; font-size: 14px;", "{filename}" }
                span {
                    style: "font-size: 11px; color: {status_color}; font-weight: 600; text-transform: uppercase;",
                    "{state}"
                }
            }

            div {
                style: "font-size: 12px; color: #9ca3af; margin-top: 4px;",
                "{format_size(size)}"
            }

            if state == "UPLOADING" {
                div {
                    style: "margin-top: 8px; height: 4px; background: #e5e7eb; border-radius: 2px; overflow: hidden;",
                    div {
                        style: "height: 100%; width: {progress}%; background: #3b82f6; transition: width 0.3s;",
                    }
                }
                div {
                    style: "font-size: 11px; color: #9ca3af; margin-top: 2px;",
                    "{progress}% — {format_size(bytes_uploaded)} / {format_size(size)}"
                }
            }

            if let Some(ref err) = error_message {
                div {
                    style: "font-size: 12px; color: #ef4444; margin-top: 4px;",
                    "{err}"
                }
            }

            for warning in warnings.iter() {
                div {
                    style: "font-size: 12px; color: #f59e0b; margin-top: 2px;",
                    "{warning}"
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
