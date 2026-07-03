//! In Progress tab: every row the engine is still working on, partitioned
//! into the prototype's three user-facing stages.
//!
//! 1. **Checking files** — quality check + hashing (reading/verifying the file).
//! 2. **In queue** — staged rows that passed checks, waiting for the next stage.
//! 3. **Uploading** — server-prep + transfer + verify + Paused. Transcoding
//!    folds in here (no user-facing transcode stage — transcode is a server
//!    concern, see D1 in `docs/UI-1TO1-PORT-PLAN.md`).
//!
//! Terminal buckets live in their own tabs (Rejected → Failed/Quality;
//! Completed/Failed → their own tabs). Rows are rendered through the shared
//! components in `rows.rs`; this module only does the per-stage partition.

use super::rows::{HashingRow, QualityCheckingRow, SectionHeader, StagedRow, UploadTaskRow};
use dioxus::prelude::*;
use lw_core::config::TranscodeConfig;
use lw_core::models::{UploadState, UploadTask};
use lw_core::video::DeviceEncoderSignature;
use std::collections::HashMap;

/// The In Progress tab body. `tasks` is the already-filtered slice (this
/// tab's bucket, after any per-project narrowing). All the action handlers
/// are threaded down from the panel so the rows stay dumb.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn InProgressList(
    tasks: Vec<UploadTask>,
    transcode_config: TranscodeConfig,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
    transcode_progress: Signal<HashMap<String, f32>>,
    upload_progress: Signal<HashMap<String, (u64, u64)>>,
    hash_progress: Signal<HashMap<String, (u64, u64)>>,
    embed_progress: Signal<HashMap<String, (u64, u64)>>,
    upload_speed: Signal<HashMap<String, f64>>,
    on_remove: EventHandler<String>,
    on_clear: EventHandler<String>,
    on_transcode_click: EventHandler<String>,
    on_fill_metadata: EventHandler<String>,
    on_skip_metadata: EventHandler<String>,
    on_retry: EventHandler<String>,
    on_pause: EventHandler<String>,
    on_resume: EventHandler<String>,
) -> Element {
    // The prototype collapses the pipeline into three user-facing stages.
    // Stage 1 — "Checking files": quality check + hashing merged (both are
    // "reading and verifying the file" from the user's point of view).
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
    // Stage 2 — "In queue": passed checks, waiting for the next stage.
    let staged: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Staged)
        .cloned()
        .collect();
    // Stage 3 — "Uploading": everything the engine is actively moving through
    // the upload pipeline (server-prep + transfer + verify), plus `Paused`
    // rows (shown in place with a Resume button — `is_active()` is false for
    // them, but the user expects the row to stay where they left it).
    //
    // Transcoding folds in here: there is NO user-facing transcode stage on
    // the desktop (transcode is a server concern — see D1). The engine may
    // still transcode, but a transcoding row simply reads as "Uploading"
    // rather than getting its own "Preparing" section.
    let uploading: Vec<_> = tasks
        .iter()
        .filter(|t| t.state.is_active() || t.state == UploadState::Paused)
        .cloned()
        .collect();

    let checking_count = quality_checking.len() + hashing.len();
    let everything_empty = checking_count == 0 && staged.is_empty() && uploading.is_empty();

    rsx! {
        if everything_empty {
            div {
                style: "text-align: center; padding: 40px 16px; color: var(--text-muted); font-size: 13px;",
                "Nothing in progress"
            }
        } else {
            // Prototype stage order (step 1 → 3): Checking files → In queue →
            // Uploading. The overview header and (later) the stage-filter pills
            // give quick access to active uploads without pinning them to the
            // top; within each section, rows keep their natural pipeline order.
            if checking_count > 0 {
                SectionHeader {
                    title: "Checking files",
                    count: checking_count,
                    subtitle: Some("Reading and verifying format, quality, and metadata.".to_string()),
                }
                div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                    for task in quality_checking.iter() {
                        QualityCheckingRow {
                            key: "{task.id}",
                            task: task.clone(),
                            on_remove,
                        }
                    }
                    for task in hashing.iter() {
                        HashingRow {
                            key: "{task.id}",
                            task: task.clone(),
                            device_encoder_signatures,
                            hash_progress,
                            on_remove,
                        }
                    }
                }
            }

            if !staged.is_empty() {
                SectionHeader {
                    title: "In queue",
                    count: staged.len(),
                    subtitle: Some("Passed checks — waiting to start the next stage.".to_string()),
                }
                div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                    for task in staged.iter() {
                        StagedRow {
                            key: "{task.id}",
                            task: task.clone(),
                            transcode_config: transcode_config.clone(),
                            device_encoder_signatures,
                            on_remove,
                            on_transcode_click,
                            on_force_upload: None,
                            on_fill_metadata,
                            on_skip_metadata,
                            embed_progress,
                        }
                    }
                }
            }

            if !uploading.is_empty() {
                SectionHeader {
                    title: "Uploading",
                    count: uploading.len(),
                    subtitle: Some("Validating and transferring original files to the cloud.".to_string()),
                }
                div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                    for task in uploading.iter() {
                        UploadTaskRow {
                            key: "{task.id}",
                            task: task.clone(),
                            transcode_progress,
                            upload_progress,
                            upload_speed,
                            on_retry,
                            on_remove: on_clear,
                            on_pause,
                            on_resume,
                        }
                    }
                }
            }
        }
    }
}
