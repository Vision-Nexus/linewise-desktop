//! In Progress tab: every row the engine is still working on, plus the
//! pre-upload staging rows (Checking / Hashing / Ready to Upload) and
//! Paused rows.
//!
//! The list keeps the section breakdown the old queue had — Checking,
//! Hashing, Ready to Upload, Preparing (transcode), Uploading — minus the
//! terminal buckets (Rejected went to Failed/Quality; Completed/Failed went
//! to their own tabs). Rows are rendered through the shared components in
//! `rows.rs`; this module only does the per-section partition.

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
    upload_speed: Signal<HashMap<String, f64>>,
    on_remove: EventHandler<String>,
    on_clear: EventHandler<String>,
    on_transcode_click: EventHandler<String>,
    on_fill_metadata: EventHandler<String>,
    on_retry: EventHandler<String>,
    on_pause: EventHandler<String>,
    on_resume: EventHandler<String>,
) -> Element {
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

    let everything_empty = quality_checking.is_empty()
        && hashing.is_empty()
        && staged.is_empty()
        && transcoding.is_empty()
        && active.is_empty();

    rsx! {
        if everything_empty {
            div {
                style: "text-align: center; padding: 40px 16px; color: var(--text-muted); font-size: 13px;",
                "Nothing in progress"
            }
        } else {
            // Section order is "closest-to-done first": actively-uploading rows
            // are pinned to the TOP (with their live "Uploading N" count), then
            // Preparing / Ready to Upload, then the back-of-queue Hashing /
            // Checking. Before this, "Uploading" rendered LAST — so when a big
            // batch was still hashing/checking, the in-flight uploads were pushed
            // below the fold and the user had to scroll to see any upload at all.
            if !active.is_empty() {
                SectionHeader { title: "Uploading", count: active.len() }
                div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                    for task in active.iter() {
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

            if !transcoding.is_empty() {
                SectionHeader { title: "Preparing", count: transcoding.len() }
                div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                    for task in transcoding.iter() {
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

            if !staged.is_empty() {
                SectionHeader { title: "Ready to Upload", count: staged.len() }
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
                            on_remove,
                        }
                    }
                }
            }

            if !quality_checking.is_empty() {
                SectionHeader { title: "Checking", count: quality_checking.len() }
                div { style: "display: flex; flex-direction: column; gap: 6px; margin-bottom: 16px;",
                    for task in quality_checking.iter() {
                        QualityCheckingRow {
                            key: "{task.id}",
                            task: task.clone(),
                            on_remove,
                        }
                    }
                }
            }
        }
    }
}
