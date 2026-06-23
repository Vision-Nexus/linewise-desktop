//! Failed tab: a secondary split between Quality rejections and Network
//! (transport) failures.
//!
//! * **Quality** = `Rejected`. These never auto-advanced to upload — the
//!   source video failed the acceptance gate. Rendered through `StagedRow`,
//!   which already shows the REJECTED chip, the error-coloured
//!   `rejection_reasons`, and (for super-admins) the Force-upload bypass.
//!   There is deliberately NO naked Retry — retrying re-runs the same
//!   verdict; the fix is a different source file or the admin override.
//! * **Network** = `Failed`. A genuine transport failure with an
//!   `error_message`. Rendered through `UploadTaskRow`, which offers a
//!   working `[Retry]` (and `[Remove]`).
//!
//! Reconciled duplicates do NOT land here — the `DuplicateDetected` arm in
//! `upload_runtime.rs` maps them to `Completed` with an "Already exists"
//! marker, so the Network sub-tab only ever holds real failures.

use super::rows::{SectionHeader, StagedRow, UploadTaskRow};
use super::tabs::{FailedTab, SubTabButton};
use dioxus::prelude::*;
use lw_core::config::TranscodeConfig;
use lw_core::models::{UploadState, UploadTask};
use lw_core::video::DeviceEncoderSignature;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
#[component]
pub fn FailedList(
    tasks: Vec<UploadTask>,
    transcode_config: TranscodeConfig,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
    transcode_progress: Signal<HashMap<String, f32>>,
    upload_progress: Signal<HashMap<String, (u64, u64)>>,
    upload_speed: Signal<HashMap<String, f64>>,
    on_remove: EventHandler<String>,
    on_clear: EventHandler<String>,
    on_transcode_click: EventHandler<String>,
    on_retry: EventHandler<String>,
    on_pause: EventHandler<String>,
    on_resume: EventHandler<String>,
    on_force_upload: EventHandler<String>,
) -> Element {
    // Secondary tab is local state — re-anchoring on Quality when the panel
    // re-mounts is fine.
    let mut sub_tab = use_signal(|| FailedTab::Quality);
    // Rejected rows never run a Save-time embed; a permanently-empty map satisfies
    // `StagedRow`'s required `embed_progress` prop.
    let embed_progress = use_signal(HashMap::new);

    let quality: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Rejected)
        .cloned()
        .collect();
    let network: Vec<_> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Failed)
        .cloned()
        .collect();

    let active = *sub_tab.read();

    rsx! {
        div {
            style: "display: flex; gap: 8px; margin-bottom: 16px;",
            SubTabButton {
                label: "Quality".to_string(),
                count: quality.len(),
                active: active == FailedTab::Quality,
                onclick: move |_| sub_tab.set(FailedTab::Quality),
            }
            SubTabButton {
                label: "Network".to_string(),
                count: network.len(),
                active: active == FailedTab::Network,
                onclick: move |_| sub_tab.set(FailedTab::Network),
            }
        }

        match active {
            FailedTab::Quality => rsx! {
                if quality.is_empty() {
                    EmptyHint { text: "No quality rejections" }
                } else {
                    SectionHeader { title: "Quality rejected", count: quality.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px;",
                        for task in quality.iter() {
                            StagedRow {
                                key: "{task.id}",
                                task: task.clone(),
                                transcode_config: transcode_config.clone(),
                                device_encoder_signatures,
                                on_remove,
                                on_transcode_click,
                                on_force_upload: Some(on_force_upload),
                                // Rejected rows never auto-upload, so the
                                // required-metadata prompt/button is suppressed
                                // (gated on `!is_rejected` in `StagedRow`).
                                on_fill_metadata: move |_: String| {},
                                // Rejected rows are never embedding; an empty map.
                                embed_progress,
                            }
                        }
                    }
                }
            },
            FailedTab::Network => rsx! {
                if network.is_empty() {
                    EmptyHint { text: "No network failures" }
                } else {
                    SectionHeader { title: "Network failed", count: network.len() }
                    div { style: "display: flex; flex-direction: column; gap: 6px;",
                        for task in network.iter() {
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
            },
        }
    }
}

#[component]
fn EmptyHint(text: &'static str) -> Element {
    rsx! {
        div {
            style: "text-align: center; padding: 40px 16px; color: var(--text-muted); font-size: 13px;",
            "{text}"
        }
    }
}
