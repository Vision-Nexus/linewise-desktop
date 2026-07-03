//! Shared row components for the transfer panel.
//!
//! These were lifted verbatim out of the old `upload_queue.rs` (behaviour
//! unchanged, just relocated and made `pub(crate)` so the per-tab view
//! modules can reuse them). The in-progress staging rows
//! (`QualityCheckingRow` / `HashingRow` / `StagedRow`) and the generic
//! `UploadTaskRow` all live here, alongside the probe-data popover and the
//! byte/duration formatters.

use crate::components::progress::{Progress, ProgressIndicator};
use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::prelude::*;
use lw_core::config::TranscodeConfig;
use lw_core::models::{UploadState, UploadTask};
use lw_core::video;
use lw_core::video::DeviceEncoderSignature;
use std::collections::HashMap;

/// Compact one-line summary of the capture metadata set for a clip, for the
/// inline "✓ set" row. Only present fields appear, joined by " · ".
fn capture_summary(m: &lw_core::capture::CaptureMetadata) -> String {
    let mut parts: Vec<String> = Vec::new();
    for v in [&m.country, &m.city, &m.site, &m.station]
        .into_iter()
        .flatten()
    {
        parts.push(v.clone());
    }
    if let Some(o) = &m.operator {
        parts.push(format!("op {o}"));
    }
    let device = [m.make.as_deref(), m.model.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if !device.is_empty() {
        parts.push(device);
    }
    if let Some(fov) = m.fov {
        parts.push(format!("{fov}\u{00B0}"));
    }
    if let Some(a) = &m.action {
        parts.push(a.clone());
    }
    parts.join(" \u{00B7} ")
}

#[component]
pub fn SectionHeader(
    title: String,
    count: usize,
    #[props(default)] subtitle: Option<String>,
) -> Element {
    rsx! {
        div {
            style: "margin-bottom: 8px;",
            div {
                style: "display: flex; align-items: center; gap: 8px;",
                span { style: "font-size: 13px; font-weight: 600; color: var(--text);", "{title}" }
                span {
                    style: "font-size: 11px; color: var(--text-secondary); background: var(--bg-tertiary); padding: 2px 8px; border-radius: 999px;",
                    "{count}"
                }
            }
            if let Some(sub) = subtitle {
                div { style: "font-size: 11px; color: var(--text-muted); margin-top: 3px;", "{sub}" }
            }
        }
    }
}

/// Row metadata derived from a `VideoInfo`: a one-line `summary` for
/// the dashed-underline affordance and three column groups for the
/// hover popover. Lifted out of `StagedRow` so the same markup can
/// render under both the `Checking → Hashing` rows (where the data
/// has just landed) and the `Staged / Rejected` rows (where it has
/// been there since staging finished).
#[derive(Clone, PartialEq)]
pub struct VideoDetails {
    summary: String,
    structural: Vec<(String, String)>,
    device: Vec<(String, String)>,
    raw: Vec<(String, String)>,
}

pub fn build_video_details(
    task: &UploadTask,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
) -> Option<VideoDetails> {
    // `.as_deref()` (not `.as_ref()`) so `info` is `&VideoInfo`, matching the
    // `&VideoInfo`-typed `video::device_info_rows` param below. Field access
    // would auto-deref through the `Arc`, but passing it on does not.
    let info = task.video_info.as_deref()?;
    let codec = info.codec.to_uppercase();
    let res = format!("{}x{}", info.width, info.height);
    let fps_text = format!("{:.0}fps", info.fps);
    let bitrate = video::format_bitrate(info.bitrate_kbps);
    let summary = format!("{codec} · {res} · {fps_text} · {bitrate}");
    let mut structural: Vec<(String, String)> = Vec::new();
    structural.push(("Codec".into(), info.codec.to_uppercase()));
    structural.push(("Resolution".into(), res));
    structural.push(("Frame rate".into(), format!("{:.2} fps", info.fps)));
    structural.push(("Bitrate".into(), bitrate));
    if !info.audio_codec.is_empty() {
        structural.push(("Audio".into(), info.audio_codec.to_uppercase()));
    }
    structural.push(("Duration".into(), format_duration(info.duration_secs)));
    structural.push(("Container".into(), info.format.clone()));

    let mut device: Vec<(String, String)> =
        video::device_info_rows(info, device_encoder_signatures)
            .into_iter()
            .map(|(label, value)| {
                let display = if value.is_empty() {
                    "\u{2014}".to_string()
                } else {
                    value
                };
                (label.to_string(), display)
            })
            .collect();
    device.push((
        "Telemetry".into(),
        info.telemetry.clone().unwrap_or_else(|| "\u{2014}".into()),
    ));

    let raw: Vec<(String, String)> = info.metadata.clone();
    Some(VideoDetails {
        summary,
        structural,
        device,
        raw,
    })
}

/// Renders the dashed-underline summary + the three-group hover
/// popover (source metadata, device, raw tags). Identical in
/// `HashingRow` and `StagedRow`; pulled out so adding a new row that
/// shows probe data doesn't need to copy 60 lines of rsx.
#[component]
pub fn VideoInfoPopover(details: VideoDetails) -> Element {
    let VideoDetails {
        summary,
        structural,
        device,
        raw,
    } = details;
    rsx! {
        div {
            class: "popover-host",
            style: "margin-top: 4px;",
            tabindex: "0",
            div {
                style: "font-size: 11px; color: var(--text-secondary); border-bottom: 1px dashed var(--border); display: inline-block;",
                "{summary}"
            }
            div {
                class: "popover-panel",
                style: if raw.is_empty() {
                    "max-height: 360px; overflow-y: auto;".to_string()
                } else {
                    "max-height: 360px; overflow-y: auto; display: grid; grid-template-columns: minmax(200px, 1fr) minmax(220px, 1fr); gap: 12px;".to_string()
                },
                div {
                    style: "min-width: 0;",
                    div {
                        style: "font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-bottom: 6px;",
                        "Source metadata"
                    }
                    div {
                        style: "display: grid; grid-template-columns: max-content 1fr; column-gap: 10px; row-gap: 3px; font-size: 11px;",
                        for (key, value) in structural.iter() {
                            div { style: "color: var(--text-muted); white-space: nowrap;", "{key}" }
                            div { style: "color: var(--text); word-break: break-all;", "{value}" }
                        }
                    }
                    div {
                        style: "font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-top: 10px; margin-bottom: 6px;",
                        "Device"
                    }
                    div {
                        style: "display: grid; grid-template-columns: max-content 1fr; column-gap: 10px; row-gap: 3px; font-size: 11px;",
                        for (key, value) in device.iter() {
                            div { style: "color: var(--text-muted); white-space: nowrap;", "{key}" }
                            div { style: "color: var(--text); word-break: break-all;", "{value}" }
                        }
                    }
                }
                if !raw.is_empty() {
                    div {
                        style: "min-width: 0;",
                        div {
                            style: "font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-bottom: 6px;",
                            "Raw tags"
                        }
                        div {
                            style: "display: grid; grid-template-columns: max-content 1fr; column-gap: 10px; row-gap: 3px; font-size: 11px;",
                            for (key, value) in raw.iter() {
                                div { style: "color: var(--text-muted); white-space: nowrap; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;", "{key}" }
                                div { style: "color: var(--text); word-break: break-all;", "{value}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One row in the "Checking files" stage (quality-check half): a freshly-added
/// video whose local atom walk + server `/quality-check` round-trip is in flight.
/// Renders an indeterminate progress bar — the network round-trip
/// has no progress signal we could surface — plus a Remove
/// affordance. Removing mid-check is safe for the same reason as
/// `HashingRow`: the worker writes to SQLite at completion, so a
/// removed row's terminal write becomes a no-op.
#[component]
pub fn QualityCheckingRow(task: UploadTask, on_remove: EventHandler<String>) -> Element {
    let task_id = task.id.clone();
    let (card_border, card_bg) = card_tone(&task.state, false);
    rsx! {
        div {
            class: "card-row fade-in",
            style: "padding: 10px 12px; border: 1px solid {card_border}; border-radius: 6px; background: {card_bg};",
            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                span {
                    style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0;",
                    "{task.filename}"
                }
                span {
                    style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0; margin-left: 8px;",
                    "{format_size(task.size)}"
                }
            }
            div {
                style: "margin-top: 6px;",
                // Prototype shows a fixed 8% sliver for quality-checking — the
                // network round-trip has no live progress signal, so a small
                // determinate bar + friendly sub-label matches the prototype's
                // `checkingProgressLabel` ("Checking files 8% — Checking your file").
                Progress {
                    value: 8.0,
                    max: 100.0,
                    "aria-label": "Quality check in progress",
                    ProgressIndicator {}
                }
            }
            div {
                style: "font-size: 11px; margin-top: 2px; color: var(--text-muted);",
                "Checking files 8% — Checking your file"
            }
            div {
                style: "display: flex; justify-content: flex-end; margin-top: 6px;",
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

/// One row in the "Checking files" stage (hashing half): a freshly-added file
/// whose BLAKE3+MD5 stream is in flight. The quality check has already
/// landed, so the row carries the same probe-data popover and
/// advisory warnings the `Staged` row will eventually show — the
/// user gets to see the verdict during the hash window instead of
/// only after it. Renders a determinate progress bar driven by
/// `HashProgress` events, plus a Remove affordance. We let the user
/// remove a row mid-hash — the worker writes to SQLite at
/// completion, so a removed row's terminal write becomes a no-op
/// when the row no longer exists.
#[component]
pub fn HashingRow(
    task: UploadTask,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
    hash_progress: Signal<HashMap<String, (u64, u64)>>,
    on_remove: EventHandler<String>,
) -> Element {
    let task_id = task.id.clone();
    let (bytes_hashed, total_bytes) = hash_progress
        .read()
        .get(&task.id)
        .copied()
        .unwrap_or((0, task.size.max(1)));
    let t = if total_bytes > 0 {
        bytes_hashed as f64 / total_bytes as f64
    } else {
        0.0
    };
    // Prototype `checkingProgressPct(hashing)` = lerp(12→100): the "Checking
    // files" bar picks up where quality-check (8%) left off. Sub-label drops
    // the byte counts (prototype presentation) — the bar carries the progress.
    let pct = (12.0 + (100.0 - 12.0) * t).round().min(100.0);
    let label = format!("Checking files {pct:.0}% — Reading your file");
    let video_details = build_video_details(&task, device_encoder_signatures);
    let warning_style = "font-size: 11px; color: var(--warning); margin-top: 4px; padding: 3px 6px; background: var(--warning-bg); border-radius: 4px;";
    let (card_border, card_bg) = card_tone(&task.state, false);

    rsx! {
        div {
            class: "card-row fade-in",
            style: "padding: 10px 12px; border: 1px solid {card_border}; border-radius: 6px; background: {card_bg};",
            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                span {
                    style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0;",
                    "{task.filename}"
                }
                span {
                    style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0; margin-left: 8px;",
                    "{format_size(task.size)}"
                }
            }
            if let Some(details) = video_details {
                VideoInfoPopover { details }
            }
            for warning in task.validation_warnings.iter() {
                div {
                    style: "{warning_style}",
                    "{warning}"
                }
            }
            div {
                style: "margin-top: 6px;",
                Progress {
                    value: pct,
                    max: 100.0,
                    "aria-label": "Hash progress",
                    ProgressIndicator {}
                }
            }
            div {
                style: "font-size: 11px; margin-top: 2px; color: var(--text-muted);",
                "{label}"
            }
            div {
                style: "display: flex; justify-content: flex-end; margin-top: 6px;",
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
pub fn StagedRow(
    task: UploadTask,
    transcode_config: TranscodeConfig,
    device_encoder_signatures: &'static [DeviceEncoderSignature],
    on_remove: EventHandler<String>,
    on_transcode_click: EventHandler<String>,
    /// Super-admin override hook. `Some` only on rows in the
    /// `Rejected` section; the regular Staged section passes `None`.
    /// Even when present, the button only renders for users whose
    /// `system_roles` contain `admin` (see `UserInfo::is_super_admin`).
    on_force_upload: Option<EventHandler<String>>,
    /// Open the per-file capture-metadata sheet for this clip. Capture is offered
    /// on a `Staged` clip until it is filled OR skipped.
    on_fill_metadata: EventHandler<String>,
    /// Skip capture metadata for this clip: it resolves the metadata gate without
    /// values, and the clip auto-advances to upload (unless transcode-held).
    on_skip_metadata: EventHandler<String>,
    /// Save-time capture-embed progress `(bytes, total)` per task; present only
    /// while this clip's metadata is being written into its file.
    embed_progress: Signal<HashMap<String, (u64, u64)>>,
) -> Element {
    let task_id = task.id.clone();
    let force_id = task.id.clone();
    let transcode_id = task.id.clone();
    let fill_id = task.id.clone();
    let skip_id = task.id.clone();
    let is_video = task.mime_type.starts_with("video/");
    let is_super_admin = use_context::<AppState>()
        .user_info
        .read()
        .as_ref()
        .map(|u| u.is_super_admin())
        .unwrap_or(false);
    let transcode_on = task.transcode;
    // Upscale guard: only show the transcode toggle when transcoding would
    // actually shrink this clip. Non-video files never get the toggle; for
    // videos without a probe yet, fall back to showing the toggle (user can
    // still opt out manually).
    let transcode_useful = task
        .video_info
        .as_deref()
        .map(|info| video::transcode_would_help(info, &transcode_config))
        .unwrap_or(true);
    // Master toggle gate: when the feature is disabled in global
    // settings, don't surface the per-task affordance at all. The
    // "Already matches targets" badge follows because it only makes
    // sense relative to a feature the user is using.
    let feature_enabled = transcode_config.enabled;
    let rejected = task.state == UploadState::Rejected;
    let show_transcode_toggle = feature_enabled && is_video && transcode_useful && !rejected;
    let show_already_ok_badge = feature_enabled && is_video && !transcode_useful && !rejected;

    // Probe data is built into the same shape — summary line + three
    // popover groups — used by `HashingRow`. Lifting it out makes the
    // hashing row light up with codec/resolution/fps the moment the
    // quality check returns, instead of waiting for the row to land
    // in `Staged`.
    let video_details = build_video_details(&task, device_encoder_signatures);

    // Required-metadata gate: a `Staged` clip whose capture metadata is neither
    // filled NOR skipped holds here until the user resolves it (fill or Skip).
    // Rejected rows never upload, so the prompt is suppressed for them. When
    // filled, the recorded values are shown inline; when skipped, a muted "skipped"
    // note shows instead. Reading `capture_rev` subscribes the row to fill/skip/
    // batch changes so it re-renders immediately (the engine's capture maps are not
    // reactive).
    let _capture_rev: u64 = *use_context::<AppState>().capture_rev.read();
    let engine = use_context::<CoreServices>().upload_engine.clone();
    let capture = engine.capture_metadata_for(&task.id);
    let skipped = engine.is_capture_skipped(&task.id);
    // Save-time embed in progress for this clip → show a determinate bar and
    // suppress the fill prompt/button until the rewrite finishes.
    let embedding = embed_progress.read().get(&task.id).copied();
    let needs_metadata =
        task.state == UploadState::Staged && capture.is_none() && !skipped && embedding.is_none();

    let btn_style = "height: 26px; padding: 0 10px; font-size: 12px; border-radius: 6px; cursor: pointer; border: 1px solid var(--border); transition: background 0.15s;";
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

    let is_rejected = task.state == UploadState::Rejected;
    // Card tint by state: staged → sky, rejected → destructive (see `card_tone`).
    let (card_border, card_bg) = card_tone(&task.state, false);
    let row_style = format!(
        "padding: 10px 12px; border: 1px solid {card_border}; border-radius: 6px; background: {card_bg}; transition: background 0.15s, border-color 0.15s;"
    );
    // Two severities, two palettes:
    //   * `warning_style` — recommend-band advisories, telemetry hints,
    //     missing-fingerprint nudges. Warn palette regardless of the
    //     row's verdict; on a rejected row they sit alongside the
    //     error-coloured reject reasons so the user can tell the
    //     "you might want to" lines from the "this won't upload" lines.
    //   * `reason_style` — acceptance-band reject reasons. Always
    //     error-coloured; only present on rejected rows.
    let warning_style = "font-size: 11px; color: var(--warning); margin-top: 4px; padding: 3px 6px; background: var(--warning-bg); border-radius: 4px;";
    let reason_style = "font-size: 11px; color: var(--error); margin-top: 4px; padding: 3px 6px; background: var(--bg); border: 1px solid var(--error); border-radius: 4px;";

    rsx! {
        div {
            class: "card-row fade-in",
            style: "{row_style}",

            // Filename + size, plus an inline REJECTED chip when applicable
            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                div {
                    style: "flex: 1; min-width: 0;",
                    div {
                        style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                        if is_rejected {
                            span {
                                style: "display: inline-block; font-size: 10px; font-weight: 600; letter-spacing: 0.05em; padding: 1px 5px; margin-right: 6px; border-radius: 4px; background: var(--error); color: white; vertical-align: 1px;",
                                "REJECTED"
                            }
                        }
                        "{task.filename}"
                    }
                }
                span { style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0; margin-left: 8px;", "{format_size(task.size)}" }
            }

            if let Some(details) = video_details.clone() {
                VideoInfoPopover { details }
            }

            // Capture metadata, when set AND not mid-write: a green confirmation
            // line showing the recorded values, so the user can see at a glance
            // which clips are filled and with what. Suppressed while embedding (the
            // bar below takes over) and absent entirely when nothing is set (the
            // "Needs metadata" warning shows instead).
            if let Some(m) = capture.as_ref().filter(|_| embedding.is_none()) {
                div {
                    style: "font-size: 11px; color: var(--success); margin-top: 4px; padding: 3px 6px; background: var(--success-bg); border-radius: 4px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                    title: "Capture metadata written into this clip's file.",
                    "\u{2713} {capture_summary(m)}"
                }
            }

            // Skipped: the user opted out of capture metadata for this clip. Shown
            // in a muted style (no values) so it reads as a resolved-but-empty state
            // distinct from the green "filled" line. "Add metadata" stays available.
            if skipped && capture.is_none() && embedding.is_none() {
                div {
                    style: "font-size: 11px; color: var(--text-muted); margin-top: 4px; padding: 3px 6px; background: var(--bg-secondary, rgba(0,0,0,0.04)); border-radius: 4px;",
                    title: "Capture metadata was skipped for this clip — it uploads without io.visionlab tags.",
                    "Capture metadata skipped"
                }
            }

            // Save-time embed in progress. A determinate bar (driven by the
            // exiftool rewrite-temp size) ONCE we have a byte count; until then —
            // or if the rewrite-temp can't be located — an honest "Writing
            // metadata…" line rather than a bar stuck at 0%.
            if let Some((bytes, total)) = embedding {
                div { style: "margin-top: 6px;",
                    if bytes > 0 {
                        Progress {
                            value: (bytes as f64 / total.max(1) as f64 * 100.0).min(100.0),
                            max: 100.0,
                            "aria-label": "Embedding metadata",
                            ProgressIndicator {}
                        }
                        div {
                            style: "font-size: 11px; margin-top: 2px; color: var(--text-muted);",
                            {format!(
                                "Writing metadata — {} / {} ({:.0}%)",
                                format_size(bytes),
                                format_size(total),
                                (bytes as f64 / total.max(1) as f64 * 100.0).min(100.0),
                            )}
                        }
                    } else {
                        div {
                            style: "font-size: 11px; color: var(--text-muted);",
                            "Writing metadata\u{2026}"
                        }
                    }
                }
            }

            // Reject reasons render first so they read as the headline
            // for a rejected row; advisory warnings follow underneath in
            // the warn palette.
            for reason in task.rejection_reasons.iter() {
                div {
                    style: "{reason_style}",
                    "{reason}"
                }
            }
            for warning in task.validation_warnings.iter() {
                div {
                    style: "{warning_style}",
                    "{warning}"
                }
            }

            // Action buttons
            div {
                style: "display: flex; justify-content: flex-end; align-items: center; gap: 6px; margin-top: 8px;",
                if needs_metadata {
                    span {
                        style: "margin-right: auto; font-size: 11px; color: var(--warning); padding: 2px 6px; border-radius: 4px; background: var(--warning-bg); border: 1px solid var(--warning);",
                        title: "Capture metadata is required before this clip uploads.",
                        "\u{26A0} Needs metadata"
                    }
                }
                if !is_rejected && embedding.is_none() {
                    button {
                        style: if needs_metadata {
                            format!("{btn_style} background: var(--btn-primary); color: white; border-color: var(--btn-primary);")
                        } else {
                            format!("{btn_style} background: transparent; color: var(--text-secondary);")
                        },
                        onclick: move |_| on_fill_metadata.call(fill_id.clone()),
                        if needs_metadata { "Add metadata" } else { "Edit metadata" }
                    }
                }
                // Skip: only offered while the clip is still unresolved. Resolving by
                // skipping auto-advances the clip to upload (capture is optional).
                if needs_metadata {
                    button {
                        style: format!("{btn_style} background: transparent; color: var(--text-secondary);"),
                        title: "Upload this clip without capture metadata.",
                        onclick: move |_| on_skip_metadata.call(skip_id.clone()),
                        "Skip"
                    }
                }
                if show_already_ok_badge {
                    span {
                        style: "font-size: 11px; color: var(--text-secondary); padding: 2px 6px; border-radius: 4px; background: var(--bg-secondary); border: 1px solid var(--border);",
                        title: "Source already at or below transcode targets — no benefit to re-encoding.",
                        "Already matches targets"
                    }
                }
                if show_transcode_toggle {
                    button {
                        style: "{transcode_btn_style}",
                        onclick: move |_| on_transcode_click.call(transcode_id.clone()),
                        "{transcode_label}"
                    }
                }
                if let (true, Some(handler)) = (is_super_admin && is_rejected, on_force_upload) {
                    button {
                        class: "btn-warning-sm",
                        style: "height: 24px; padding: 0 8px; font-size: 11px; border-radius: 4px; cursor: pointer; background: var(--warning); color: white; border: 1px solid var(--warning);",
                        title: "Bypass dedup and quality checks for this file. Visible to super-admins only.",
                        onclick: move |_| handler.call(force_id.clone()),
                        "Force upload"
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

#[allow(clippy::too_many_arguments)]
#[component]
pub fn UploadTaskRow(
    task: UploadTask,
    transcode_progress: Signal<HashMap<String, f32>>,
    upload_progress: Signal<HashMap<String, (u64, u64)>>,
    /// Per-task UI-derived upload speed (bytes/sec). Absent or `<= 0` means
    /// unknown — no rate/ETA is shown. Only consulted while `Uploading`.
    upload_speed: Signal<HashMap<String, f64>>,
    on_retry: EventHandler<String>,
    on_remove: EventHandler<String>,
    on_pause: EventHandler<String>,
    on_resume: EventHandler<String>,
) -> Element {
    let app_state = use_context::<AppState>();

    // The bytes-actually-uploaded denominator: transcoded size when we have
    // it, otherwise the original. GCS only ever sees one of these on the wire.
    let upload_total = task.transcoded_size.unwrap_or(task.size);

    // Phase-aware progress reader. Read from the live signals only — never
    // from the cloned task's `bytes_uploaded`, which lags behind and snaps
    // back to 0 on render races.
    // Prototype-aligned stage percents (wave `uploadStageProgressPct`): the
    // upload stage shows fixed markers for the server-prep sub-states and a
    // lerp(22→96) for the actual transfer, so the bar never sits at 0% while
    // the engine is clearly working. Transcoding folds into "Uploading" and
    // borrows the transcode %, shown without any transcode wording (D1).
    let (progress_pct, uploaded_bytes) = match task.state {
        UploadState::Completed => (100u32, upload_total),
        UploadState::Transcoding => {
            let pct = transcode_progress
                .read()
                .get(&task.id)
                .copied()
                .unwrap_or(0.0) as u32;
            (pct.min(100), 0u64)
        }
        UploadState::Uploading | UploadState::Verifying | UploadState::Paused => {
            let uploaded = upload_progress
                .read()
                .get(&task.id)
                .map(|&(u, _)| u)
                .unwrap_or(0);
            (upload_stage_pct(&task.state, uploaded, upload_total), uploaded)
        }
        UploadState::Pending | UploadState::Validating | UploadState::Creating => {
            (upload_stage_pct(&task.state, 0, upload_total), 0)
        }
        _ => (0, 0),
    };

    // Already-exists rows (Completed + the reconcile marker) render as an amber
    // "Already exists" chip instead of a plain "Completed".
    let already_exists = task.error_message.as_deref() == Some(super::ALREADY_EXISTS_MARKER);
    let (card_border, card_bg) = card_tone(&task.state, already_exists);
    let (badge_bg, badge_fg) = badge_tone(&task.state, already_exists);

    // Collapse the engine's fine-grained state to the user-facing stage badge
    // (mirrors the prototype `displayStatusLabel`), so the chip reads
    // "Uploading" instead of internal jargon like "TRANSCODING"/"CREATING".
    let badge_label = if already_exists {
        "Already exists"
    } else {
        stage_badge_label(&task.state)
    };

    let tenant_name = app_state.tenant_display_name(&task.tenant_id);
    let project_name = app_state.project_display_name(&task.tenant_id, &task.project_id);

    // Size column: "original" for non-transcoded tasks, "original → transcoded"
    // when transcode is enabled. The transcoded half is "…" until the engine
    // emits TranscodeCompleted.
    let size_line = if task.transcode {
        match task.transcoded_size {
            Some(tr) => format!("{} → {}", format_size(task.size), format_size(tr)),
            None => format!("{} → …", format_size(task.size)),
        }
    } else {
        format_size(task.size)
    };

    // Stall hint: driven by REAL multipart part-retry state, not a byte-progress
    // timeout. The engine emits `PartRetrying` from the part PUT retry loop and
    // the runtime records the latest attempt in `part_retrying`; presence means a
    // part is currently failing and backing off. A landed part (`Progress`) or
    // any transition out of `Uploading` clears the entry. This avoids the false
    // "stalled" the old `last_progress_at + STALL_THRESHOLD` heuristic produced
    // on healthy big-file uploads — the backend hands big files 64 MiB parts and
    // MPU progress only fires per completed part, so no progress for tens of
    // seconds is normal, not a stall. No UI ticker — this recomputes on the next
    // PartRetrying/Progress/StateChanged re-render, exactly when it can flip.
    let stalled = if task.state == UploadState::Uploading {
        app_state.part_retrying.read().get(&task.id).copied()
    } else {
        None
    };

    rsx! {
        div {
            class: "card-row",
            style: "padding: 10px 12px; border: 1px solid {card_border}; border-radius: 6px; background: {card_bg}; transition: background 0.15s, border-color 0.15s, box-shadow 0.15s;",

            div {
                style: "display: flex; justify-content: space-between; align-items: center;",
                div {
                    style: "flex: 1; min-width: 0;",
                    span {
                        style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block;",
                        "{task.filename}"
                    }
                    span {
                        style: "font-size: 11px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block; margin-top: 2px;",
                        "{tenant_name} / {project_name}"
                    }
                }
                div {
                    style: "display: flex; align-items: center; gap: 6px; margin-left: 8px; flex-shrink: 0;",
                    span {
                        style: "display: inline-flex; align-items: center; height: 16px; padding: 0 6px; \
                                font-size: 10px; font-weight: 500; border-radius: 4px; \
                                background: {badge_bg}; color: {badge_fg}; white-space: nowrap;",
                        "{badge_label}"
                    }
                    // Action buttons based on state
                    {
                        let id1 = task.id.clone();
                        let id2 = task.id.clone();
                        let small_btn = "display: inline-flex; align-items: center; gap: 5px; height: 26px; padding: 0 10px; font-size: 12px; border-radius: 6px; cursor: pointer; transition: background 0.15s, transform 0.08s;";
                        match task.state {
                            UploadState::Uploading
                            | UploadState::Validating
                            | UploadState::Transcoding
                            | UploadState::Creating
                            | UploadState::Verifying
                            | UploadState::Pending => rsx! {
                                button {
                                    class: "btn-outline",
                                    style: "{small_btn} background: transparent; color: var(--warning); border: 1px solid var(--warning);",
                                    onclick: move |_| on_pause.call(id1.clone()),
                                    crate::icons::PauseIcon {}
                                    "Pause"
                                }
                            },
                            UploadState::Paused => rsx! {
                                button {
                                    class: "btn-primary",
                                    style: "{small_btn} background: var(--btn-primary); color: white; border: none;",
                                    onclick: move |_| on_resume.call(id1.clone()),
                                    crate::icons::PlayIcon {}
                                    "Resume"
                                }
                                button {
                                    class: "btn-danger-sm",
                                    style: "{small_btn} background: transparent; color: var(--error); border: 1px solid var(--error);",
                                    onclick: move |_| on_remove.call(id2.clone()),
                                    crate::icons::TrashIcon {}
                                    "Remove"
                                }
                            },
                            // Failed and GaveUp both offer manual Retry + Remove.
                            // Retrying a GaveUp row resets its durable retry_count
                            // and re-enters the normal flow (see `on_retry`).
                            UploadState::Failed | UploadState::GaveUp => rsx! {
                                button {
                                    class: "btn-primary",
                                    style: "{small_btn} background: var(--btn-primary); color: white; border: none;",
                                    onclick: move |_| on_retry.call(id1.clone()),
                                    crate::icons::RetryIcon {}
                                    "Retry"
                                }
                                button {
                                    class: "btn-danger-sm",
                                    style: "{small_btn} background: transparent; color: var(--error); border: 1px solid var(--error);",
                                    onclick: move |_| on_remove.call(id2.clone()),
                                    crate::icons::TrashIcon {}
                                    "Remove"
                                }
                            },
                            UploadState::Completed => rsx! {
                                button {
                                    class: "btn-outline",
                                    style: "{small_btn} background: transparent; color: var(--text); border: 1px solid var(--border);",
                                    onclick: move |_| on_remove.call(id1.clone()),
                                    crate::icons::TrashIcon {}
                                    "Clear"
                                }
                            },
                            UploadState::QualityChecking
                            | UploadState::Staged
                            | UploadState::Rejected
                            | UploadState::Hashing => rsx! {},
                        }
                    }
                }
            }

            div {
                style: "font-size: 12px; color: var(--text-muted); margin-top: 4px;",
                "{size_line}"
            }

            {
                let show_progress = task.state.is_active() || task.state == UploadState::Paused;
                let phase_label = phase_label(&task.state, progress_pct, uploaded_bytes, upload_total);
                // Real-time rate + ETA, derived UI-side from successive Progress
                // events (see `upload_runtime::sample_upload_speed`). Only on
                // an Uploading row with a known positive rate; other states
                // (Verifying/Transcoding/…) show no rate. During a stalled
                // chunk the last computed rate persists until the next Progress
                // event refreshes it — there is no UI ticker (P1 scope).
                let bps = if task.state == UploadState::Uploading {
                    upload_speed.read().get(&task.id).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
                let speed_label = speed_eta_label(bps, uploaded_bytes, upload_total);
                let bar_wrapper_style = if show_progress {
                    "margin-top: 6px;"
                } else {
                    "display: none;"
                };
                let label_style = if show_progress {
                    "font-size: 11px; margin-top: 2px; color: var(--text-muted);"
                } else {
                    "display: none;"
                };
                let paused_class = if task.state == UploadState::Paused {
                    "progress-paused"
                } else {
                    ""
                };
                let value = progress_pct as f64;
                rsx! {
                    div {
                        style: "{bar_wrapper_style}",
                        Progress {
                            class: paused_class,
                            value,
                            max: 100.0,
                            "aria-label": "Upload progress",
                            ProgressIndicator {}
                        }
                    }
                    div {
                        style: "{label_style}",
                        "{phase_label}"
                        if !speed_label.is_empty() {
                            span {
                                style: "color: var(--text-secondary); margin-left: 8px;",
                                "{speed_label}"
                            }
                        }
                    }
                }
            }

            if let Some(attempt) = stalled {
                div {
                    style: "font-size: 12px; color: var(--warning); margin-top: 4px; padding: 6px 8px; background: var(--warning-bg); border-radius: 4px;",
                    "Connection stalled — retrying (attempt {attempt})…"
                }
            }

            // Only while the row is still being auto-retried — a terminal
            // `GaveUp` row keeps `retry_count` at the cap but must show its
            // give-up message, not a contradictory "Retrying…" line.
            if task.retry_count > 0 && task.state != UploadState::GaveUp {
                div {
                    style: "font-size: 12px; color: var(--warning); margin-top: 4px; padding: 6px 8px; background: var(--warning-bg); border-radius: 4px;",
                    "Retrying after a network error (attempt {task.retry_count})…"
                }
            }

            if let Some(ref err) = task.error_message {
                div {
                    style: "display: flex; align-items: flex-start; gap: 6px; font-size: 12px; color: var(--error); margin-top: 4px; padding: 6px 8px; background: var(--error-bg); border-radius: 4px;",
                    crate::icons::AlertTriangleIcon {}
                    span { "{err}" }
                }
            }

            for reason in task.rejection_reasons.iter() {
                div {
                    style: "display: flex; align-items: flex-start; gap: 6px; font-size: 12px; color: var(--error); margin-top: 2px; padding: 4px 8px; background: var(--error-bg); border-radius: 4px;",
                    crate::icons::AlertTriangleIcon {}
                    span { "{reason}" }
                }
            }
            for warning in task.validation_warnings.iter() {
                div {
                    style: "display: flex; align-items: flex-start; gap: 6px; font-size: 12px; color: var(--warning); margin-top: 2px; padding: 4px 8px; background: var(--warning-bg); border-radius: 4px;",
                    crate::icons::AlertTriangleIcon {}
                    span { "{warning}" }
                }
            }
        }
    }
}

/// Phase label rendered under the progress bar, mirroring the prototype's
/// `uploadStageProgressLabel` + `uploadStageActivityLabel`: every upload-stage
/// row reads "Uploading N% — <friendly activity>", so the user sees one stage
/// with a plain-language sub-line instead of raw engine states. Transcoding
/// folds in here with a neutral "Preparing your file" — no transcode wording,
/// per D1. The rate/ETA suffix is appended separately by the caller.
fn phase_label(state: &UploadState, pct: u32, uploaded: u64, total: u64) -> String {
    match state {
        UploadState::Transcoding => format!("Uploading {pct}% — Preparing your file"),
        UploadState::Pending => format!("Uploading {pct}% — Waiting to start"),
        UploadState::Validating => format!("Uploading {pct}% — Confirming file details"),
        UploadState::Creating => format!("Uploading {pct}% — Setting up your upload"),
        UploadState::Uploading => format!(
            "Uploading {pct}% — Transferring to the cloud — {} / {}",
            format_size(uploaded),
            format_size(total)
        ),
        UploadState::Verifying => format!("Uploading {pct}% — Finishing up"),
        UploadState::Paused => format!(
            "Uploading {pct}% — Paused · {} / {}",
            format_size(uploaded),
            format_size(total)
        ),
        UploadState::QualityChecking
        | UploadState::Hashing
        | UploadState::Staged
        | UploadState::Rejected
        | UploadState::Completed
        | UploadState::Failed
        | UploadState::GaveUp => String::new(),
    }
}

/// 0–100% progress within the "Uploading" stage — fixed markers for the
/// server-prep sub-states, `lerp(22→96)` for the actual transfer. Mirrors the
/// prototype `uploadStageProgressPct`. Transcoding is handled by the caller
/// (it borrows the live transcode %), so it returns 0 here.
fn upload_stage_pct(state: &UploadState, uploaded: u64, total: u64) -> u32 {
    match state {
        UploadState::Pending => 2,
        UploadState::Validating => 10,
        UploadState::Creating => 18,
        UploadState::Uploading | UploadState::Paused => {
            let t = if total > 0 {
                uploaded as f64 / total as f64
            } else {
                0.0
            };
            (22.0 + (96.0 - 22.0) * t).round() as u32
        }
        UploadState::Verifying => 99,
        UploadState::Transcoding
        | UploadState::QualityChecking
        | UploadState::Hashing
        | UploadState::Staged
        | UploadState::Rejected
        | UploadState::Completed
        | UploadState::Failed
        | UploadState::GaveUp => 0,
    }
}

/// User-facing stage badge — mirrors the prototype `displayStatusLabel` /
/// `userFacingStatusLabel`. The engine's fine-grained pipeline states collapse
/// to the three user-facing stages, so the chip never shows internal jargon
/// like "TRANSCODING" or "CREATING". (`already exists` refinement for Completed
/// lives in the Completed tab; here Completed simply reads "Completed".)
fn stage_badge_label(state: &UploadState) -> &'static str {
    match state {
        UploadState::QualityChecking | UploadState::Hashing => "Checking files",
        UploadState::Staged => "In queue",
        UploadState::Pending
        | UploadState::Validating
        | UploadState::Transcoding
        | UploadState::Creating
        | UploadState::Uploading
        | UploadState::Verifying
        | UploadState::Paused => "Uploading",
        UploadState::Rejected => "Rejected",
        UploadState::Completed => "Completed",
        UploadState::Failed | UploadState::GaveUp => "Failed",
    }
}

/// Prototype `cardTone`: the row's `(border, background)` tint by stage. Uses
/// low-alpha accent tints (mirroring the wave `bg-sky-500/[0.03]` approach) so
/// the same values read correctly over both the light and dark app background.
/// `already_exists` — a completed row whose marker says the content was already
/// on the server — takes the amber tone. Checking and the upload stage stay
/// neutral (Plan A's primary is neutral, so a "primary tint" would be invisible);
/// the accent tints are reserved for the states the prototype colours.
fn card_tone(state: &UploadState, already_exists: bool) -> (&'static str, &'static str) {
    if already_exists {
        return ("rgba(245,158,11,0.28)", "rgba(245,158,11,0.06)");
    }
    match state {
        UploadState::QualityChecking
        | UploadState::Hashing
        | UploadState::Pending
        | UploadState::Validating
        | UploadState::Transcoding
        | UploadState::Creating
        | UploadState::Uploading
        | UploadState::Verifying => ("var(--border)", "var(--bg-secondary)"),
        UploadState::Staged => ("rgba(59,130,246,0.25)", "rgba(59,130,246,0.05)"),
        UploadState::Paused => ("rgba(245,158,11,0.28)", "rgba(245,158,11,0.06)"),
        UploadState::Completed => ("rgba(34,197,94,0.25)", "rgba(34,197,94,0.05)"),
        UploadState::Failed | UploadState::GaveUp | UploadState::Rejected => {
            ("rgba(239,68,68,0.28)", "rgba(239,68,68,0.05)")
        }
    }
}

/// Prototype `statusBadgeClass`: the badge pill's `(background, foreground)`.
/// Same accent family as [`card_tone`], a touch stronger so the chip reads as a
/// pill. Checking/upload-stage use a neutral chip; the coloured accents match
/// the prototype for staged (sky), paused/already-exists (amber), completed
/// (emerald) and failed/rejected (destructive).
fn badge_tone(state: &UploadState, already_exists: bool) -> (&'static str, &'static str) {
    if already_exists {
        return ("rgba(245,158,11,0.15)", "var(--warning)");
    }
    match state {
        UploadState::QualityChecking | UploadState::Hashing => {
            ("var(--bg-tertiary)", "var(--text-secondary)")
        }
        UploadState::Staged => ("rgba(59,130,246,0.15)", "var(--info)"),
        UploadState::Pending
        | UploadState::Validating
        | UploadState::Transcoding
        | UploadState::Creating
        | UploadState::Uploading
        | UploadState::Verifying => ("var(--bg-tertiary)", "var(--text)"),
        UploadState::Paused => ("rgba(245,158,11,0.15)", "var(--warning)"),
        UploadState::Completed => ("rgba(34,197,94,0.15)", "var(--success)"),
        UploadState::Failed | UploadState::GaveUp | UploadState::Rejected => {
            ("rgba(239,68,68,0.12)", "var(--error)")
        }
    }
}

/// Build the "rate + ETA" suffix shown beside the upload phase label, e.g.
/// `"2.0 MB/s · ETA 0:45"`. Returns an empty string when the rate is unknown
/// (`bps <= 0`), so the caller renders nothing. ETA is appended only when the
/// total size is known and positive; remaining bytes use a saturating
/// subtraction so a slightly-over-total reading reads as `ETA 0:00`, not a
/// wild value.
fn speed_eta_label(bps: f64, uploaded: u64, total: u64) -> String {
    if bps <= 0.0 {
        return String::new();
    }
    let rate = format!("{}/s", format_size(bps as u64));
    if total == 0 {
        return rate;
    }
    let remaining = total.saturating_sub(uploaded);
    let eta_secs = remaining as f64 / bps;
    format!("{rate} \u{00b7} ETA {}", format_duration(eta_secs))
}

pub fn format_size(bytes: u64) -> String {
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

fn format_duration(secs: f64) -> String {
    let secs = secs.max(0.0);
    let total = secs.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
