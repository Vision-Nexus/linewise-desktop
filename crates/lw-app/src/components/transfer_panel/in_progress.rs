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

/// Pipeline order for the flat "All" list (mirrors the prototype's
/// PIPELINE_STATE_ORDER): checks → queue → server-prep → transfer → verify.
fn pipeline_order(state: &UploadState) -> u8 {
    match state {
        UploadState::QualityChecking => 0,
        UploadState::Hashing => 1,
        UploadState::Staged => 2,
        UploadState::Pending => 3,
        UploadState::Validating => 4,
        UploadState::Transcoding => 5,
        UploadState::Creating => 6,
        UploadState::Uploading => 7,
        UploadState::Paused => 8,
        UploadState::Verifying => 9,
        UploadState::Completed
        | UploadState::Rejected
        | UploadState::Failed
        | UploadState::GaveUp => 99,
    }
}

/// Renders the correct row component for a task's state. Used for both the flat
/// "All" list and the per-stage lists so the dispatch lives in one place.
#[allow(clippy::too_many_arguments)]
#[component]
fn InProgressRow(
    task: UploadTask,
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
    match task.state {
        UploadState::QualityChecking => rsx! {
            QualityCheckingRow { task: task.clone(), on_remove }
        },
        UploadState::Hashing => rsx! {
            HashingRow {
                task: task.clone(),
                device_encoder_signatures,
                hash_progress,
                on_remove,
            }
        },
        UploadState::Staged => rsx! {
            StagedRow {
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
        },
        UploadState::Pending
        | UploadState::Validating
        | UploadState::Transcoding
        | UploadState::Creating
        | UploadState::Uploading
        | UploadState::Verifying
        | UploadState::Paused => rsx! {
            UploadTaskRow {
                task: task.clone(),
                transcode_progress,
                upload_progress,
                upload_speed,
                on_retry,
                on_remove: on_clear,
                on_pause,
                on_resume,
            }
        },
        UploadState::Completed
        | UploadState::Rejected
        | UploadState::Failed
        | UploadState::GaveUp => rsx! {},
    }
}

/// The In Progress tab body. `tasks` is the already-filtered slice (this tab's
/// bucket, scoped to the selected batch). All the action handlers are threaded
/// down from the panel so the rows stay dumb.
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

    let mut stage_filter = use_signal(|| StageFilter::All);
    let filter = *stage_filter.read();

    // Active list + section header by filter. `All` is ONE flat list in pipeline
    // order (matching the prototype — not stacked per-stage sections); a specific
    // stage shows just that stage's rows.
    let (section_title, section_subtitle, active_list): (&str, &str, Vec<UploadTask>) = match filter
    {
        StageFilter::All => {
            let mut all: Vec<UploadTask> = tasks
                .iter()
                .filter(|t| {
                    matches!(
                        t.state,
                        UploadState::QualityChecking | UploadState::Hashing | UploadState::Staged
                    ) || t.state.is_active()
                        || t.state == UploadState::Paused
                })
                .cloned()
                .collect();
            all.sort_by_key(|t| pipeline_order(&t.state));
            (
                "In progress",
                "Automatic stages from checks through upload, in pipeline order.",
                all,
            )
        }
        StageFilter::Checking => {
            let mut v = quality_checking.clone();
            v.extend(hashing.iter().cloned());
            (
                "Checking files",
                "Reading and verifying format, quality, and metadata.",
                v,
            )
        }
        StageFilter::Queued => (
            "In queue",
            "Passed checks — waiting to start the next stage.",
            staged.clone(),
        ),
        StageFilter::Uploading => (
            "Uploading",
            "Validating and transferring original files to the cloud.",
            uploading.clone(),
        ),
    };
    let empty_msg = match filter {
        StageFilter::All => "Nothing in progress",
        StageFilter::Checking => "Nothing in checking",
        StageFilter::Queued => "Nothing in queue",
        StageFilter::Uploading => "Nothing uploading",
    };

    rsx! {
        if total_count == 0 {
            div {
                style: "text-align: center; padding: 40px 16px; color: var(--text-muted); font-size: 13px;",
                "Nothing in progress"
            }
        } else {
            // Stage-filter pills — All ▸ [1] Checking files ▸ [2] In queue ▸ [3]
            // Uploading. "All" is a single flat pipeline-ordered list.
            StageFilterPills {
                filter,
                total: total_count,
                checking: checking_count,
                queued: staged_count,
                uploading: uploading_count,
                on_select: move |f| stage_filter.set(f),
            }
            if active_list.is_empty() {
                div {
                    style: "text-align: center; padding: 32px 16px; color: var(--text-muted); font-size: 13px;",
                    "{empty_msg}"
                }
            } else {
                SectionHeader {
                    title: section_title.to_string(),
                    count: active_list.len(),
                    subtitle: Some(section_subtitle.to_string()),
                }
                div { style: "display: flex; flex-direction: column; gap: 6px;",
                    for task in active_list.iter() {
                        InProgressRow {
                            key: "{task.id}",
                            task: task.clone(),
                            transcode_config: transcode_config.clone(),
                            device_encoder_signatures,
                            transcode_progress,
                            upload_progress,
                            hash_progress,
                            embed_progress,
                            upload_speed,
                            on_remove,
                            on_clear,
                            on_transcode_click,
                            on_fill_metadata,
                            on_skip_metadata,
                            on_retry,
                            on_pause,
                            on_resume,
                        }
                    }
                }
            }
        }
    }
}
