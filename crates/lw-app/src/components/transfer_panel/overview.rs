//! Batch upload overview — a pure-derived summary of every task in the panel
//! (overall %, "X of N videos", bytes, ETA, aggregate speed) plus a segmented
//! progress bar. Mirrors the wave prototype's `computeBatchUploadSummary` /
//! `computeBatchFillSegments` / `BatchUploadOverview`.
//!
//! Nothing here touches the engine: every number is derived from the
//! already-owned task list plus the live progress/speed maps the resident
//! `UploadRuntime` maintains. The overall % is byte-weighted (matching the
//! prototype's live component, which uses `overallProgressPct`, not the
//! deprecated file-count `filesCompletePct`).

use super::rows::{checking_stage_pct, format_size, upload_stage_pct};
use super::tabs::PrimaryTab;
use dioxus::prelude::*;
use lw_core::models::{UploadState, UploadTask};
use std::collections::HashMap;

/// Derived, render-ready batch summary. Plain numbers so it is cheap to compute
/// once per render and pass down as a memoizable prop.
#[derive(Clone, PartialEq)]
pub struct BatchSummary {
    pub total_files: usize,
    pub completed_files: usize,
    pub in_progress_files: usize,
    pub failed_files: usize,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub remaining_bytes: u64,
    /// Byte-weighted overall progress (0–100) — drives the headline % and the bar.
    pub overall_progress_pct: u32,
    pub aggregate_speed_bps: f64,
    pub estimated_secs_remaining: Option<f64>,
    /// No files left in checking, queue, or the upload pipeline.
    pub batch_finished: bool,
    // In-progress stage tallies (for the segmented fill bar).
    pub checking: usize,
    pub queued: usize,
    pub uploading: usize,
}

/// 0–100% across the full per-file pipeline (checks → queue → upload), used to
/// weight a task's transferred bytes. Mirrors the prototype
/// `taskPipelineProgressPct`: checking occupies 0–8%, queue sits at 10%, and the
/// upload stage spans 10–~99%.
fn task_pipeline_pct(
    task: &UploadTask,
    hashed: u64,
    hash_total: u64,
    uploaded: u64,
    upload_total: u64,
) -> u32 {
    match task.state {
        UploadState::Completed => 100,
        UploadState::Failed | UploadState::GaveUp | UploadState::Rejected => 0,
        UploadState::QualityChecking | UploadState::Hashing => {
            (checking_stage_pct(&task.state, hashed, hash_total) as f64 * 0.08).round() as u32
        }
        UploadState::Staged => 10,
        UploadState::Pending
        | UploadState::Validating
        | UploadState::Transcoding
        | UploadState::Creating
        | UploadState::Uploading
        | UploadState::Verifying
        | UploadState::Paused => (10.0
            + upload_stage_pct(&task.state, uploaded, upload_total) as f64 * 0.9)
            .round() as u32,
    }
}

/// Aggregate the panel's task list into a [`BatchSummary`]. Reads the same
/// progress/speed maps the rows read, so the overview's byte weighting stays
/// consistent with the per-row bars.
pub fn compute_summary(
    tasks: &[UploadTask],
    upload_progress: &HashMap<String, (u64, u64)>,
    hash_progress: &HashMap<String, (u64, u64)>,
    upload_speed: &HashMap<String, f64>,
) -> BatchSummary {
    let mut completed_files = 0usize;
    let mut in_progress_files = 0usize;
    let mut failed_files = 0usize;
    let mut total_bytes = 0u64;
    let mut transferred_bytes = 0u64;
    let mut aggregate_speed_bps = 0.0f64;
    let (mut checking, mut queued, mut uploading) = (0usize, 0usize, 0usize);

    for task in tasks {
        let total = task.size;
        let (hashed, hash_total) = hash_progress
            .get(&task.id)
            .copied()
            .unwrap_or((0, total.max(1)));
        let (uploaded, upload_total) = upload_progress.get(&task.id).copied().unwrap_or((0, total));

        // Byte weighting: pipeline % of this file's size counts as transferred.
        // `task_pipeline_pct` already returns 100 for completed and 0 for
        // failed/rejected, so no special-casing is needed here.
        let pct = task_pipeline_pct(task, hashed, hash_total, uploaded, upload_total);
        let done = ((total as u128 * pct as u128) / 100) as u64;
        total_bytes += total;
        transferred_bytes += done;

        if PrimaryTab::Completed.contains(&task.state) {
            completed_files += 1;
        } else if PrimaryTab::Failed.contains(&task.state) {
            failed_files += 1;
        } else {
            in_progress_files += 1;
            match task.state {
                UploadState::QualityChecking | UploadState::Hashing => checking += 1,
                UploadState::Staged => queued += 1,
                UploadState::Pending
                | UploadState::Validating
                | UploadState::Transcoding
                | UploadState::Creating
                | UploadState::Uploading
                | UploadState::Verifying
                | UploadState::Paused => uploading += 1,
                // Classified as completed/failed above — unreachable here.
                UploadState::Completed
                | UploadState::Failed
                | UploadState::GaveUp
                | UploadState::Rejected => {}
            }
        }

        if task.state == UploadState::Uploading
            && let Some(&spd) = upload_speed.get(&task.id)
            && spd > 0.0
        {
            aggregate_speed_bps += spd;
        }
    }

    let remaining_bytes = total_bytes.saturating_sub(transferred_bytes);
    let overall_progress_pct = if total_bytes > 0 {
        ((transferred_bytes as f64 / total_bytes as f64) * 100.0).round() as u32
    } else {
        0
    };
    let batch_finished = in_progress_files == 0;
    let estimated_secs_remaining = if aggregate_speed_bps > 0.0 && remaining_bytes > 0 {
        Some(remaining_bytes as f64 / aggregate_speed_bps)
    } else {
        None
    };

    BatchSummary {
        total_files: tasks.len(),
        completed_files,
        in_progress_files,
        failed_files,
        total_bytes,
        transferred_bytes,
        remaining_bytes,
        overall_progress_pct,
        aggregate_speed_bps,
        estimated_secs_remaining,
        batch_finished,
        checking,
        queued,
        uploading,
    }
}

/// One coloured stripe inside the byte-progress fill; widths sum to 100% of the
/// fill. Mirrors `computeBatchFillSegments`.
struct FillSegment {
    label: &'static str,
    count: usize,
    width_pct: f64,
    color: &'static str,
}

fn fill_segments(s: &BatchSummary) -> Vec<FillSegment> {
    if s.total_files == 0 {
        return Vec::new();
    }
    // Prototype colour order: completed(emerald) / uploading(primary) /
    // checking(muted) / queued(sky) / failed(destructive). Plan A's primary is
    // neutral, so "uploading" uses the near-black text colour.
    let defs = [
        ("completed", s.completed_files, "var(--success)"),
        ("uploading", s.uploading, "var(--text)"),
        ("checking", s.checking, "var(--text-muted)"),
        ("in queue", s.queued, "var(--info)"),
        ("failed", s.failed_files, "var(--error)"),
    ];
    defs.iter()
        .filter(|(_, count, _)| *count > 0)
        .map(|(label, count, color)| FillSegment {
            label,
            count: *count,
            width_pct: (*count as f64 / s.total_files as f64) * 100.0,
            color,
        })
        .collect()
}

/// Human ETA: hours+minutes so a long estimate is never ambiguous. Mirrors the
/// prototype `formatEtaDuration` ("1h 5m" / "5m 30s" / "30s").
fn format_eta_long(secs: f64) -> String {
    let total = secs.max(0.0).floor() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// The ` · `-joined meta line under the headline. Mirrors `buildMetaLine`.
fn build_meta_line(s: &BatchSummary) -> String {
    let mut parts: Vec<String> = vec![
        format!("{} of {} videos", s.completed_files, s.total_files),
        format!(
            "{} / {}",
            format_size(s.transferred_bytes),
            format_size(s.total_bytes)
        ),
    ];
    if !s.batch_finished {
        if let Some(secs) = s.estimated_secs_remaining {
            parts.push(format!("Est. {} left", format_eta_long(secs)));
        } else if s.in_progress_files > 0 {
            parts.push("Estimating time…".to_string());
        }
    }
    if s.aggregate_speed_bps > 0.0 {
        parts.push(format!("{}/s", format_size(s.aggregate_speed_bps as u64)));
    }
    parts.join(" \u{00B7} ")
}

/// The batch overview card. Renders nothing when there are no tasks (mirrors the
/// prototype's `totalFiles === 0 → null`).
#[component]
pub fn BatchOverview(summary: BatchSummary) -> Element {
    if summary.total_files == 0 {
        return rsx! {};
    }

    let is_complete = summary.batch_finished;
    let has_active = summary.in_progress_files > 0;
    let segments = fill_segments(&summary);
    let meta = build_meta_line(&summary);

    let container_style = if is_complete {
        "border: 1px solid var(--border); background: var(--bg-secondary); border-radius: 6px; \
         padding: 6px 10px; margin-bottom: 12px;"
    } else {
        "border: 1px solid var(--border); background: var(--bg-secondary); border-radius: 8px; \
         padding: 8px 12px; margin-bottom: 12px; box-shadow: 0 0 0 1px rgba(0,0,0,0.03);"
    };
    let meta_style = if is_complete {
        "margin: 4px 0 0; font-size: 10px; line-height: 1.3; color: var(--text-muted); \
         overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"
    } else {
        "margin: 6px 0 0; font-size: 11px; line-height: 1.35; color: var(--text-muted); \
         overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"
    };
    let pct = summary.overall_progress_pct;

    rsx! {
        section {
            style: "{container_style}",
            "aria-label": "Batch upload progress",

            div {
                style: "display: flex; align-items: center; justify-content: space-between; gap: 8px;",
                p {
                    style: "margin: 0; font-size: 12px; font-weight: 500; line-height: 1;",
                    if is_complete && summary.failed_files == 0 {
                        span {
                            style: "display: inline-flex; align-items: center; gap: 4px; color: var(--success);",
                            crate::icons::CheckCircleIcon {}
                            span { style: "color: var(--text);", "Batch complete" }
                        }
                    } else if is_complete {
                        span {
                            style: "display: inline-flex; align-items: center; gap: 4px; flex-wrap: wrap;",
                            span { "Batch complete" }
                            span {
                                style: "font-size: 10px; font-weight: 400; color: var(--text-muted);",
                                "\u{00B7} {summary.completed_files} succeeded \u{00B7} {summary.failed_files} failed"
                            }
                        }
                    } else {
                        span {
                            style: "display: inline-flex; align-items: baseline; gap: 4px;",
                            span { style: "font-variant-numeric: tabular-nums;", "{pct}%" }
                            span { style: "font-weight: 400; color: var(--text-muted);", "progress" }
                        }
                    }
                }
                if has_active && !is_complete {
                    span {
                        style: "display: inline-flex; color: var(--text-muted); flex-shrink: 0;",
                        crate::icons::SpinnerIcon {}
                    }
                }
            }

            if !is_complete {
                div {
                    style: "margin-top: 8px; position: relative; height: 6px; width: 100%; \
                            overflow: hidden; border-radius: 999px; background: var(--bg-tertiary);",
                    "role": "img",
                    "aria-label": "{pct}% of batch data in progress",
                    if pct > 0 {
                        div {
                            style: "position: absolute; top: 0; bottom: 0; left: 0; display: flex; \
                                    overflow: hidden; border-radius: 999px; width: {pct}%;",
                            for seg in segments.iter() {
                                div {
                                    style: "height: 100%; min-width: 1px; flex-shrink: 0; width: {seg.width_pct}%; background: {seg.color};",
                                    title: "{seg.count} {seg.label}",
                                }
                            }
                        }
                    }
                }
            }

            if !meta.is_empty() {
                p {
                    style: "{meta_style}",
                    title: "{meta}",
                    "{meta}"
                }
            }
        }
    }
}

/// Which scope a rolled-up nav-dot summarizes. Only affects the "complete"
/// tooltip wording (a batch reads "Batch complete", an org reads "All batches
/// complete"); the dot colour/pulse and the in-progress tooltip are identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavScope {
    Batch,
    Org,
}

/// Render-ready details for a rolled-up status dot. `None` from [`nav_status`]
/// means idle — the caller renders no dot at all.
#[derive(Clone, Debug, PartialEq)]
pub struct NavStatusDetails {
    /// CSS custom-property colour token for the dot fill.
    pub color: &'static str,
    /// Whether the dot animates (the in-progress `lw-ping` pulse).
    pub pulse: bool,
    /// Hover tooltip summarising the rolled-up counts.
    pub tooltip: String,
}

/// Roll a task slice up into a nav-dot status. Scope-agnostic: pass a
/// batch-scoped slice (filtered to one tenant+project) for a batch dot, or an
/// org-scoped slice (filtered to one tenant, any project) for an org dot.
///
/// Returns `None` when the slice is empty (idle → render no dot). Mirrors the
/// prototype's `getBatchNavStatus`: in-progress wins, then failed, then
/// complete. Classification uses [`PrimaryTab::contains`] — the single source
/// of truth also used by the tab counts — so a dot can never disagree with the
/// tabs. Pure state tally (no progress maps): the dot only changes on state
/// transitions, so callers re-render on `upload_tasks` changes, not byte ticks.
pub fn nav_status(tasks: &[UploadTask], scope: NavScope) -> Option<NavStatusDetails> {
    if tasks.is_empty() {
        return None;
    }
    let (mut completed, mut failed, mut in_progress) = (0usize, 0usize, 0usize);
    for t in tasks {
        if PrimaryTab::Completed.contains(&t.state) {
            completed += 1;
        } else if PrimaryTab::Failed.contains(&t.state) {
            failed += 1;
        } else {
            in_progress += 1;
        }
    }
    let total = tasks.len();
    // Wording differs only on the terminal ("complete") tooltips.
    let complete_lead = match scope {
        NavScope::Batch => "Batch complete",
        NavScope::Org => "All batches complete",
    };

    if in_progress > 0 {
        return Some(NavStatusDetails {
            color: "var(--info)",
            pulse: true,
            tooltip: format!("{completed} of {total} videos \u{00B7} {in_progress} in progress"),
        });
    }
    if failed > 0 {
        return Some(NavStatusDetails {
            color: "var(--error)",
            pulse: false,
            tooltip: format!(
                "{complete_lead} \u{00B7} {completed} succeeded \u{00B7} {failed} failed"
            ),
        });
    }
    Some(NavStatusDetails {
        color: "var(--success)",
        pulse: false,
        tooltip: format!("{complete_lead} \u{00B7} {completed} of {total} videos"),
    })
}

/// The rolled-up status dot: an 8px colour dot with an optional `lw-ping`
/// pulse and a hover tooltip. Shared by the sidebar org rows, the sidebar
/// batch rows, and the org-landing batch cards so all three read from the same
/// [`nav_status`] output and look identical.
#[component]
pub fn NavDotView(details: NavStatusDetails) -> Element {
    rsx! {
        span {
            style: "position: relative; display: inline-flex; width: 8px; height: 8px; flex-shrink: 0;",
            title: "{details.tooltip}",
            if details.pulse {
                span {
                    class: "lw-ping",
                    style: "position: absolute; inset: 0; border-radius: 999px; background: {details.color};",
                }
            }
            span {
                style: "position: relative; width: 8px; height: 8px; border-radius: 999px; background: {details.color};",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lw_core::models::UploadState;

    fn task(state: UploadState) -> UploadTask {
        UploadTask {
            id: "t".to_string(),
            local_path: "/x".to_string(),
            filename: "f.mp4".to_string(),
            size: 100,
            mime_type: "video/mp4".to_string(),
            tenant_id: "org".to_string(),
            project_id: "proj".to_string(),
            document_id: None,
            session_id: None,
            mpu_upload_id: None,
            bytes_uploaded: 0,
            state,
            error_message: None,
            hash: None,
            source_md5: None,
            source_crc32c: None,
            source_sha256_head_256kib: None,
            validation_warnings: Vec::new(),
            rejection_reasons: Vec::new(),
            retry_count: 0,
            transcode: false,
            transcoded_size: None,
            video_info: None,
            force_upload: false,
            created_at: "2026-06-28 22:32:15".to_string(),
            updated_at: "2026-06-28 22:32:15".to_string(),
        }
    }

    #[test]
    fn nav_status_idle_when_no_tasks() {
        assert!(nav_status(&[], NavScope::Batch).is_none());
        assert!(nav_status(&[], NavScope::Org).is_none());
    }

    #[test]
    fn nav_status_in_progress_wins_over_failed_and_complete() {
        let tasks = [
            task(UploadState::Uploading),
            task(UploadState::Failed),
            task(UploadState::Completed),
        ];
        let d = nav_status(&tasks, NavScope::Batch).expect("some dot");
        assert_eq!(d.color, "var(--info)");
        assert!(d.pulse);
        assert_eq!(d.tooltip, "1 of 3 videos \u{00B7} 1 in progress");
    }

    #[test]
    fn nav_status_failed_when_no_in_progress() {
        let tasks = [
            task(UploadState::Completed),
            task(UploadState::Failed),
            task(UploadState::Rejected),
        ];
        let d = nav_status(&tasks, NavScope::Batch).expect("some dot");
        assert_eq!(d.color, "var(--error)");
        assert!(!d.pulse);
        assert_eq!(
            d.tooltip,
            "Batch complete \u{00B7} 1 succeeded \u{00B7} 2 failed"
        );
    }

    #[test]
    fn nav_status_complete_when_all_done() {
        let tasks = [task(UploadState::Completed), task(UploadState::Completed)];
        let d = nav_status(&tasks, NavScope::Batch).expect("some dot");
        assert_eq!(d.color, "var(--success)");
        assert!(!d.pulse);
        assert_eq!(d.tooltip, "Batch complete \u{00B7} 2 of 2 videos");
    }

    #[test]
    fn nav_status_org_scope_changes_complete_wording() {
        let done = [task(UploadState::Completed)];
        assert_eq!(
            nav_status(&done, NavScope::Org).expect("some").tooltip,
            "All batches complete \u{00B7} 1 of 1 videos"
        );
        let with_fail = [task(UploadState::Completed), task(UploadState::GaveUp)];
        assert_eq!(
            nav_status(&with_fail, NavScope::Org).expect("some").tooltip,
            "All batches complete \u{00B7} 1 succeeded \u{00B7} 1 failed"
        );
    }
}
