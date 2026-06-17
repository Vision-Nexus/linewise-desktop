//! Right-side sheet that configures per-task transcode settings.
//!
//! Extracted from `upload_queue.rs` to keep that module under the 500-line
//! ceiling documented in the desktop CLAUDE.md. The dialog is mounted
//! unconditionally by `UploadQueue` so the sheet's slide-out animation can
//! play on close; visibility is driven by the `open` prop.

use crate::components::sheet::{
    Sheet, SheetContent, SheetFooter, SheetHeader, SheetSide, SheetTitle,
};
use crate::components::toggle_group::{ToggleGroup, ToggleItem};
use crate::state::AppState;
use dioxus::prelude::*;

const PRESETS: &[&str] = &["fast", "medium", "slow"];
const RESOLUTIONS: &[(u32, &str)] = &[(720, "720p"), (1080, "1080p")];
const AUDIO_BITRATES: &[u32] = &[128, 192];
const FPS_OPTIONS: &[(u32, &str)] = &[(24, "24fps"), (30, "30fps"), (60, "60fps")];

#[component]
pub fn TranscodeDialog(task_id: String, open: bool, on_close: EventHandler<bool>) -> Element {
    let mut app_state = use_context::<AppState>();
    let mut config = use_signal(|| app_state.config.read().transcode.clone());

    // Find the task to show estimated output size
    // Deref the `Arc`-wrapped stored field into a plain owned `VideoInfo` so the
    // `&VideoInfo`-typed `estimate_transcoded_size` call below works unchanged.
    // One clone here is fine — it runs only when the transcode sheet opens.
    let task_info = app_state
        .upload_tasks
        .read()
        .iter()
        .find(|t| t.id == task_id)
        .and_then(|t| t.video_info.as_deref().cloned());

    let estimated = task_info
        .as_ref()
        .map(|info| lw_core::transcode::estimate_transcoded_size(info, &config.read()));

    let on_ok = move |_| {
        let mut next = app_state.config.read().clone();
        next.transcode = config.read().clone();
        if let Err(e) = app_state.save_config(next) {
            tracing::error!("Failed to save transcode config: {e}");
        }
        on_close.call(true);
    };

    let label_style = "font-size: 12px; font-weight: 500; color: var(--text); margin-bottom: 4px; display: block;";
    let src_height = task_info.as_ref().map(|i| i.height).unwrap_or(u32::MAX);
    let src_fps = task_info.as_ref().map(|i| i.fps as u32).unwrap_or(u32::MAX);
    let current_bitrate = config.read().max_bitrate_mbps;

    rsx! {
        Sheet {
            open,
            on_open_change: move |is_open: bool| {
                // The sheet primitive fires this on ESC, overlay click, or the
                // built-in close button. Treat any such close as "cancel" —
                // save-with-enable is only on the explicit Enable button.
                if !is_open {
                    on_close.call(false);
                }
            },
            SheetContent {
                side: SheetSide::Right,
                SheetHeader {
                    SheetTitle { "Transcode Settings" }
                    if let Some(est) = estimated {
                        div {
                            style: "font-size: 12px; color: var(--text-secondary); margin-top: 4px;",
                            "Estimated output: ~{format_size(est)}"
                        }
                    }
                }

                div {
                    class: "sheet-body",

                    // Preset
                    div {
                        style: "margin-bottom: 12px;",
                        label { style: label_style, "Speed" }
                        ButtonGroup {
                            options: PRESETS.iter().map(|p| (p.to_string(), p.to_string(), true)).collect(),
                            selected: config.read().preset.clone(),
                            on_select: move |v: String| config.write().preset = v,
                        }
                    }

                    // Resolution (disable above source)
                    div {
                        style: "margin-bottom: 12px;",
                        label { style: label_style, "Resolution" }
                        ButtonGroup {
                            options: RESOLUTIONS
                                .iter()
                                .map(|(h, l)| (h.to_string(), l.to_string(), *h <= src_height))
                                .collect(),
                            selected: config.read().max_height.to_string(),
                            on_select: move |v: String| {
                                if let Ok(h) = v.parse::<u32>() {
                                    config.write().max_height = h;
                                }
                            },
                        }
                    }

                    // FPS (disable above source)
                    div {
                        style: "margin-bottom: 12px;",
                        label { style: label_style, "Frame Rate" }
                        {
                            let mut fps_opts: Vec<(String, String, bool)> =
                                vec![("0".to_string(), "Original".to_string(), true)];
                            fps_opts.extend(
                                FPS_OPTIONS
                                    .iter()
                                    .map(|(f, l)| (f.to_string(), l.to_string(), *f <= src_fps)),
                            );
                            rsx! {
                                ButtonGroup {
                                    options: fps_opts,
                                    selected: config.read().target_fps.to_string(),
                                    on_select: move |v: String| {
                                        if let Ok(f) = v.parse::<u32>() {
                                            config.write().target_fps = f;
                                        }
                                    },
                                }
                            }
                        }
                    }

                    // Max bitrate — range slider (5–20 Mbps, recommend 10)
                    {
                        let (bitrate_color, bitrate_hint) = if (7..=15).contains(&current_bitrate) {
                            ("var(--success)", "Recommended")
                        } else if current_bitrate <= 6 {
                            ("var(--error)", "Low quality")
                        } else {
                            ("var(--warning)", "Large file size")
                        };
                        rsx! {
                            div {
                                style: "margin-bottom: 12px;",
                                label {
                                    style: label_style,
                                    "Max Bitrate: "
                                    span { style: "color: {bitrate_color};", "{current_bitrate} Mbps" }
                                    span { style: "font-size: 10px; color: {bitrate_color}; margin-left: 6px; font-weight: 400;", "({bitrate_hint})" }
                                }
                                input {
                                    r#type: "range",
                                    min: "5",
                                    max: "20",
                                    value: "{current_bitrate}",
                                    onchange: move |evt: Event<FormData>| {
                                        if let Ok(v) = evt.value().parse::<u32>() {
                                            config.write().max_bitrate_mbps = v;
                                        }
                                    },
                                    style: "width: 100%; accent-color: {bitrate_color};",
                                }
                                div {
                                    style: "display: flex; justify-content: space-between; font-size: 10px; color: var(--text-muted);",
                                    span { "5 Mbps" }
                                    span { "20 Mbps" }
                                }
                            }
                        }
                    }

                    // Audio bitrate
                    div {
                        style: "margin-bottom: 14px;",
                        label { style: label_style, "Audio Bitrate" }
                        ButtonGroup {
                            options: AUDIO_BITRATES
                                .iter()
                                .map(|r| (r.to_string(), format!("{r}k"), true))
                                .collect(),
                            selected: config.read().audio_bitrate_kbps.to_string(),
                            on_select: move |v: String| {
                                if let Ok(r) = v.parse::<u32>() {
                                    config.write().audio_bitrate_kbps = r;
                                }
                            },
                        }
                    }
                }

                SheetFooter {
                    button {
                        style: "flex: 1; padding: 7px 14px; border-radius: 6px; border: none; background: var(--btn-primary); color: white; cursor: pointer; font-weight: 500; font-size: 13px;",
                        onclick: on_ok,
                        "Enable Transcode"
                    }
                    button {
                        style: "padding: 7px 14px; border-radius: 6px; border: 1px solid var(--border); background: transparent; color: var(--text-secondary); cursor: pointer; font-size: 13px;",
                        onclick: move |_| on_close.call(false),
                        "Cancel"
                    }
                }
            }
        }
    }
}

/// Segmented single-select control built on `dioxus_primitives::toggle_group`
/// in radio mode. Each option is `(value, label, enabled)`. The caller keeps
/// its familiar `selected: String` + `on_select: EventHandler<String>` API;
/// this facade translates to the primitive's index-based `HashSet<usize>`
/// under the hood so the rest of the dialog stays unchanged.
///
/// Force-select invariant: if the caller's `selected` does not match any
/// option's value, we fire `on_select` with the first enabled option's value
/// on first render so the caller's state and the UI stay in sync. This also
/// handles the "un-press the current selection" path from the primitive,
/// which would otherwise leave the row with an empty selection.
#[component]
fn ButtonGroup(
    options: Vec<(String, String, bool)>,
    selected: String,
    on_select: EventHandler<String>,
) -> Element {
    // Map selected string → option index. If nothing matches, fall back to
    // the first enabled option and push that back to the caller so the data
    // model always has a concrete, enabled value.
    let initial_idx = options.iter().position(|(v, _, _)| *v == selected);
    let effective_idx = initial_idx.or_else(|| options.iter().position(|(_, _, enabled)| *enabled));

    // Force-select path: selected didn't resolve → tell the caller what we
    // picked instead. Fires once per mismatched render; harmless because the
    // caller's write updates `selected`, which matches on the next render.
    if initial_idx.is_none()
        && let Some(idx) = effective_idx
        && let Some((value, _, _)) = options.get(idx)
    {
        on_select.call(value.clone());
    }

    let pressed_set: std::collections::HashSet<usize> = effective_idx.into_iter().collect();
    let options_for_cb = options.clone();
    rsx! {
        ToggleGroup {
            horizontal: true,
            allow_multiple_pressed: false,
            pressed: Some(pressed_set),
            on_pressed_change: move |set: std::collections::HashSet<usize>| {
                // Radio mode: the primitive enforces at most one pressed item.
                // If the user un-presses the current item, `set` is empty —
                // force-reselect the first enabled option instead of leaving
                // an invalid empty state. The caller's data model (preset,
                // resolution, fps, audio bitrate) requires a concrete value.
                let target_idx = set
                    .iter()
                    .next()
                    .copied()
                    .or_else(|| options_for_cb.iter().position(|(_, _, en)| *en));
                if let Some(idx) = target_idx
                    && let Some((value, _, _)) = options_for_cb.get(idx)
                {
                    on_select.call(value.clone());
                }
            },
            for (idx, (_, label, enabled)) in options.iter().enumerate() {
                ToggleItem {
                    index: idx,
                    disabled: !*enabled,
                    "{label}"
                }
            }
        }
    }
}

/// Human-friendly byte formatter used by the transcode dialog's estimated-
/// output line. Shared helper — the upload-row renderer has its own copy in
/// `upload_queue.rs`.
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
