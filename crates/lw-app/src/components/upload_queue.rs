use crate::state::AppState;
use dioxus::prelude::*;


#[component]
pub fn UploadQueue() -> Element {
    let app_state = use_context::<AppState>();
    let tasks = app_state.upload_tasks.read();

    rsx! {
        div { class: "upload-queue",
            style: "padding: 16px;",

            div {
                style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px;",
                h2 { "Upload Queue" }
                button {
                    style: "padding: 8px 16px; background: #2563eb; color: white; border: none; border-radius: 4px; cursor: pointer;",
                    onclick: move |_| {
                        // TODO: Open file picker and queue files
                        tracing::info!("Add files clicked");
                    },
                    "Add Files"
                }
            }

            if tasks.is_empty() {
                div {
                    style: "text-align: center; padding: 48px; color: #666;",
                    p { "No files in queue" }
                    p { style: "font-size: 14px;",
                        "Drop files here or click \"Add Files\" to start uploading"
                    }
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
            style: "padding: 12px; border: 1px solid #e5e7eb; border-radius: 8px; background: #fafafa;",

            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                span { style: "font-weight: 500;", "{filename}" }
                span {
                    style: "font-size: 12px; color: {status_color}; font-weight: 600;",
                    "{state}"
                }
            }

            div {
                style: "font-size: 12px; color: #666; margin-top: 4px;",
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
                    style: "font-size: 11px; color: #888; margin-top: 2px;",
                    "{progress}% — {format_size(bytes_uploaded)} / {format_size(size)}"
                }
            }

            if let Some(ref err) = error_message {
                div {
                    style: "font-size: 12px; color: #ef4444; margin-top: 4px;",
                    "{err}"
                }
            }

            if !warnings.is_empty() {
                div { style: "margin-top: 4px;",
                    for warning in warnings.iter() {
                        div {
                            style: "font-size: 12px; color: #f59e0b;",
                            "⚠ {warning}"
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
