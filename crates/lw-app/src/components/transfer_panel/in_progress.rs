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

/// Which in-progress stage the filter pills are narrowed to. `All` stacks every
/// stage (the default); the others show just that one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StageFilter {
    All,
    Checking,
    Queued,
    Uploading,
}

/// One filter pill: an optional step-number badge, a label, and a live count.
#[component]
fn StagePill(
    label: String,
    count: usize,
    #[props(default)] num: Option<u8>,
    active: bool,
    on_click: EventHandler<()>,
) -> Element {
    let (bg, border, color) = if active {
        ("var(--bg-secondary)", "var(--border-hover)", "var(--text)")
    } else {
        ("transparent", "var(--border)", "var(--text-secondary)")
    };
    rsx! {
        button {
            style: "display: inline-flex; align-items: center; gap: 6px; height: 28px; padding: 0 10px; \
                    border-radius: 999px; cursor: pointer; font-size: 12px; font-weight: 500; \
                    background: {bg}; border: 1px solid {border}; color: {color};",
            "aria-pressed": "{active}",
            onclick: move |_| on_click.call(()),
            if let Some(n) = num {
                span {
                    style: "display: inline-flex; align-items: center; justify-content: center; \
                            width: 16px; height: 16px; border-radius: 999px; background: var(--bg-tertiary); \
                            color: var(--text-secondary); font-size: 10px; font-weight: 600;",
                    "{n}"
                }
            }
            span { "{label}" }
            span { style: "color: var(--text-muted); font-size: 11px;", "{count}" }
        }
    }
}

/// The stage-filter pill row: `All ▸ [1] Checking files ▸ [2] In queue ▸ [3] Uploading`.
#[component]
fn StageFilterPills(
    filter: StageFilter,
    total: usize,
    checking: usize,
    queued: usize,
    uploading: usize,
    on_select: EventHandler<StageFilter>,
) -> Element {
    rsx! {
        div {
            style: "display: flex; align-items: center; gap: 4px; flex-wrap: wrap; margin-bottom: 12px;",
            StagePill {
                label: "All",
                count: total,
                active: filter == StageFilter::All,
                on_click: move |_| on_select.call(StageFilter::All),
            }
            span { style: "display: inline-flex; color: var(--text-muted);", crate::icons::ChevronRightIcon {} }
            StagePill {
                label: "Checking files",
                count: checking,
                num: Some(1u8),
                active: filter == StageFilter::Checking,
                on_click: move |_| on_select.call(StageFilter::Checking),
            }
            span { style: "display: inline-flex; color: var(--text-muted);", crate::icons::ChevronRightIcon {} }
            StagePill {
                label: "In queue",
                count: queued,
                num: Some(2u8),
                active: filter == StageFilter::Queued,
                on_click: move |_| on_select.call(StageFilter::Queued),
            }
            span { style: "display: inline-flex; color: var(--text-muted);", crate::icons::ChevronRightIcon {} }
            StagePill {
                label: "Uploading",
                count: uploading,
                num: Some(3u8),
                active: filter == StageFilter::Uploading,
                on_click: move |_| on_select.call(StageFilter::Uploading),
            }
        }
    }
}

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
    let staged_count = staged.len();
    let uploading_count = uploading.len();
    let total_count = checking_count + staged_count + uploading_count;

    // Stage-filter pills (E2): `All` stacks every stage; the others narrow to one.
    let mut stage_filter = use_signal(|| StageFilter::All);
    let filter = *stage_filter.read();
    let show_checking =
        matches!(filter, StageFilter::All | StageFilter::Checking) && checking_count > 0;
    let show_queued = matches!(filter, StageFilter::All | StageFilter::Queued) && staged_count > 0;
    let show_uploading =
        matches!(filter, StageFilter::All | StageFilter::Uploading) && uploading_count > 0;
    // Message when a specific stage is selected but empty (the batch is not).
    let filtered_empty_msg: Option<&str> = match filter {
        StageFilter::All => None,
        StageFilter::Checking => (checking_count == 0).then_some("Nothing in checking"),
        StageFilter::Queued => (staged_count == 0).then_some("Nothing in queue"),
        StageFilter::Uploading => (uploading_count == 0).then_some("Nothing uploading"),
    };

    rsx! {
        if total_count == 0 {
            div {
                style: "text-align: center; padding: 40px 16px; color: var(--text-muted); font-size: 13px;",
                "Nothing in progress"
            }
        } else {
            // Stage-filter pills (All ▸ [1] Checking files ▸ [2] In queue ▸ [3]
            // Uploading). Selecting a stage narrows to it; "All" stacks all three
            // in pipeline order (Checking → In queue → Uploading).
            StageFilterPills {
                filter,
                total: total_count,
                checking: checking_count,
                queued: staged_count,
                uploading: uploading_count,
                on_select: move |f| stage_filter.set(f),
            }
            if let Some(msg) = filtered_empty_msg {
                div {
                    style: "text-align: center; padding: 32px 16px; color: var(--text-muted); font-size: 13px;",
                    "{msg}"
                }
            }
            if show_checking {
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

            if show_queued {
                SectionHeader {
                    title: "In queue",
                    count: staged_count,
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

            if show_uploading {
                SectionHeader {
                    title: "Uploading",
                    count: uploading_count,
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
