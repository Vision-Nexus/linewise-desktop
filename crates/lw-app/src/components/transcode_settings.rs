use crate::components::slider::{Slider, SliderRange, SliderThumb, SliderTrack};
use crate::components::switch::{Switch, SwitchThumb};
use crate::components::toggle_group::{ToggleGroup, ToggleItem};
use crate::state::{AppState, ToastKind};
use dioxus::prelude::*;
use lw_core::config::TranscodeConfig;
use lw_core::transcode::probe_availability;
use std::collections::HashSet;

const PRESETS: &[&str] = &["fast", "medium", "slow"];
const RESOLUTIONS: &[(u32, &str)] = &[(720, "720p"), (1080, "1080p")];
const AUDIO_BITRATES: &[u32] = &[128, 192];

#[component]
pub fn TranscodeSettingsPane() -> Element {
    let mut app_state = use_context::<AppState>();
    // Seed local edit-state from the live config signal — anything the
    // user persists via Save flows back through `AppState::save_config`.
    let mut config = use_signal(|| app_state.config.read().transcode.clone());
    // Probe ffmpeg + HW encoders once on mount. Safe to re-probe since
    // `ffmpeg_next::init()` is idempotent — but caching keeps the UI snappy.
    let availability = use_signal(|| probe_availability(&config.read()));
    let ffmpeg_ok = availability.read().ffmpeg;
    // If ffmpeg is missing, force-disable the master toggle regardless of what
    // the on-disk config says. The user can't enable transcoding without it.
    if !ffmpeg_ok && config.read().enabled {
        config.write().enabled = false;
    }
    let master_enabled = ffmpeg_ok && config.read().enabled;

    let save = move |_| {
        let tc = config.read().clone();
        let mut next = app_state.config.read().clone();
        next.transcode = tc;
        match app_state.save_config(next) {
            Ok(()) => {
                tracing::info!("Transcode settings saved");
                app_state.show_toast("Settings saved", ToastKind::Success);
            }
            Err(e) => {
                tracing::error!("Failed to save config: {e}");
                app_state.show_toast(format!("Failed to save settings: {e}"), ToastKind::Error);
            }
        }
    };

    let reset = move |_| {
        config.set(TranscodeConfig::default());
    };

    let hw_options: Vec<(String, String)> = {
        let avail = availability.read();
        std::iter::once(("auto".to_string(), "Auto".to_string()))
            .chain(std::iter::once((
                "none".to_string(),
                "Software".to_string(),
            )))
            .chain(avail.available_hw.iter().map(|hw| {
                (
                    hw.as_config_str().to_string(),
                    hw.display_label().to_string(),
                )
            }))
            .collect()
    };
    let preset_options: Vec<(String, String)> = PRESETS
        .iter()
        .map(|p| (p.to_string(), p.to_string()))
        .collect();
    let resolution_options: Vec<(u32, String)> = RESOLUTIONS
        .iter()
        .map(|(h, label)| (*h, label.to_string()))
        .collect();
    let audio_options: Vec<(u32, String)> = AUDIO_BITRATES
        .iter()
        .map(|r| (*r, format!("{r}k")))
        .collect();

    rsx! {
        div {
            style: "background: var(--bg); color: var(--text);",

            // Enable toggle — disabled when ffmpeg is absent from the system.
            SettingRow {
                label: "Enable Transcoding",
                Switch {
                    checked: config.read().enabled,
                    disabled: !ffmpeg_ok,
                    aria_label: "Enable transcoding",
                    on_checked_change: move |v: bool| {
                        if !ffmpeg_ok { return; }
                        config.write().enabled = v;
                    },
                    SwitchThumb {}
                }
            }

            if !ffmpeg_ok {
                div {
                    style: "margin: -4px 0 12px 0; padding: 8px 10px; border-radius: 4px; background: var(--warning-bg); border: 1px solid var(--warning); font-size: 12px; color: var(--warning);",
                    "ffmpeg not detected on this system — install it via your package manager (Homebrew, apt, winget) to enable transcoding."
                }
            }

            // Hardware acceleration — sub-setting of the master toggle.
            SettingRow {
                label: "Hardware Acceleration",
                StringToggleRow {
                    options: hw_options,
                    value: config.read().hw_accel.clone(),
                    disabled: !master_enabled,
                    on_change: move |v: String| config.write().hw_accel = v,
                }
            }

            // Preset
            SettingRow {
                label: "Encoding Preset",
                StringToggleRow {
                    options: preset_options,
                    value: config.read().preset.clone(),
                    disabled: !master_enabled,
                    on_change: move |v: String| config.write().preset = v,
                }
            }

            // Target average bitrate (the VBR target).
            BitrateSliderRow {
                label: "Target Bitrate",
                value: config.read().target_bitrate_mbps,
                disabled: !master_enabled,
                on_change: move |v: u32| config.write().target_bitrate_mbps = v,
            }

            // Peak cap. Typical 2× target; at equal values VideoToolbox
            // systematically undershoots the target.
            BitrateSliderRow {
                label: "Peak Bitrate Cap",
                value: config.read().max_bitrate_mbps,
                disabled: !master_enabled,
                on_change: move |v: u32| config.write().max_bitrate_mbps = v,
            }

            // Max resolution
            SettingRow {
                label: "Max Resolution",
                U32ToggleRow {
                    options: resolution_options,
                    value: config.read().max_height,
                    disabled: !master_enabled,
                    on_change: move |v: u32| config.write().max_height = v,
                }
            }

            // Audio bitrate
            SettingRow {
                label: "Audio Bitrate",
                U32ToggleRow {
                    options: audio_options,
                    value: config.read().audio_bitrate_kbps,
                    disabled: !master_enabled,
                    on_change: move |v: u32| config.write().audio_bitrate_kbps = v,
                }
            }

            // Actions
            div {
                style: "display: flex; gap: 8px; margin-top: 16px;",
                button {
                    style: "flex: 1; padding: 8px 16px; border-radius: 6px; border: none; background: var(--btn-primary); color: white; cursor: pointer; font-weight: 500; font-size: 13px;",
                    onclick: save,
                    "Save"
                }
                button {
                    style: "padding: 8px 16px; border-radius: 6px; border: 1px solid var(--border); background: transparent; color: var(--text-secondary); cursor: pointer; font-size: 13px;",
                    onclick: reset,
                    "Reset"
                }
            }

        }
    }
}

#[component]
fn SettingRow(label: &'static str, children: Element) -> Element {
    rsx! {
        div {
            style: "margin-bottom: 12px;",
            label {
                style: "display: block; font-size: 13px; font-weight: 500; margin-bottom: 4px; color: var(--text);",
                "{label}"
            }
            {children}
        }
    }
}

// --- ToggleGroup wrappers ----------------------------------------------------
// ToggleGroup's `pressed` API is index-based. We generate a row per option,
// map the current config value back to its index, and on click look up the
// pressed index to find the new value. One wrapper per value type because
// Dioxus `#[component]` doesn't yet support generic props with closures.

#[component]
fn StringToggleRow(
    options: Vec<(String, String)>,
    value: String,
    disabled: bool,
    on_change: EventHandler<String>,
) -> Element {
    let pressed: HashSet<usize> = options
        .iter()
        .position(|(v, _)| v == &value)
        .map(|i| HashSet::from([i]))
        .unwrap_or_default();
    let opts_for_callback = options.clone();

    rsx! {
        ToggleGroup {
            horizontal: true,
            disabled,
            pressed,
            on_pressed_change: move |set: HashSet<usize>| {
                if let Some(&i) = set.iter().next()
                    && let Some((v, _)) = opts_for_callback.get(i)
                {
                    on_change.call(v.clone());
                }
            },
            for (i, (_, label)) in options.iter().enumerate() {
                ToggleItem {
                    index: i,
                    "{label}"
                }
            }
        }
    }
}

#[component]
fn U32ToggleRow(
    options: Vec<(u32, String)>,
    value: u32,
    disabled: bool,
    on_change: EventHandler<u32>,
) -> Element {
    let pressed: HashSet<usize> = options
        .iter()
        .position(|(v, _)| *v == value)
        .map(|i| HashSet::from([i]))
        .unwrap_or_default();
    let opts_for_callback = options.clone();

    rsx! {
        ToggleGroup {
            horizontal: true,
            disabled,
            pressed,
            on_pressed_change: move |set: HashSet<usize>| {
                if let Some(&i) = set.iter().next()
                    && let Some((v, _)) = opts_for_callback.get(i)
                {
                    on_change.call(*v);
                }
            },
            for (i, (_, label)) in options.iter().enumerate() {
                ToggleItem {
                    index: i,
                    "{label}"
                }
            }
        }
    }
}

#[component]
fn BitrateSliderRow(
    label: &'static str,
    value: u32,
    disabled: bool,
    on_change: EventHandler<u32>,
) -> Element {
    rsx! {
        div {
            style: "margin-bottom: 12px;",
            div {
                style: "display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 4px;",
                span {
                    style: "font-size: 13px; font-weight: 500; color: var(--text);",
                    "{label}"
                }
                span {
                    style: "font-size: 12px; color: var(--text-secondary); font-variant-numeric: tabular-nums;",
                    "{value} Mbps"
                }
            }
            Slider {
                default_value: value as f64,
                min: 1.0,
                max: 100.0,
                step: 1.0,
                disabled,
                label: label.to_string(),
                on_value_change: move |v: f64| on_change.call(v.round() as u32),
                SliderTrack {
                    SliderRange {}
                    SliderThumb { index: 0usize }
                }
            }
        }
    }
}
