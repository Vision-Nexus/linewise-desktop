#![allow(dead_code)]

use dioxus::prelude::*;
use lw_core::config::{AppConfig, TranscodeConfig};

const PRESETS: &[&str] = &["fast", "medium", "slow"];
const RESOLUTIONS: &[(u32, &str)] = &[(720, "720p"), (1080, "1080p")];
const AUDIO_BITRATES: &[u32] = &[128, 192];

#[component]
pub fn TranscodeSettings(on_close: EventHandler<()>) -> Element {
    let mut config = use_signal(|| AppConfig::load().map(|c| c.transcode).unwrap_or_default());
    let mut saved = use_signal(|| false);

    let save = move |_| {
        let tc = config.read().clone();
        match AppConfig::load() {
            Ok(mut app_config) => {
                app_config.transcode = tc;
                if let Err(e) = app_config.save() {
                    tracing::error!("Failed to save config: {e}");
                } else {
                    saved.set(true);
                    tracing::info!("Transcode settings saved");
                }
            }
            Err(e) => tracing::error!("Failed to load config for save: {e}"),
        }
    };

    let reset = move |_| {
        config.set(TranscodeConfig::default());
        saved.set(false);
    };

    let close = on_close;

    rsx! {
        div {
            style: "padding: 16px; background: var(--bg); color: var(--text); max-width: 400px;",

            // Header
            div {
                style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px;",
                h3 { style: "margin: 0; font-size: 16px; font-weight: 600;", "Transcode Settings" }
                button {
                    style: "background: none; border: none; color: var(--text-muted); cursor: pointer; font-size: 18px; padding: 4px;",
                    onclick: move |_| close.call(()),
                    "×"
                }
            }

            // Enable toggle
            SettingRow {
                label: "Enable Transcoding",
                div {
                    style: "display: flex; align-items: center;",
                    input {
                        r#type: "checkbox",
                        checked: config.read().enabled,
                        onchange: move |_| {
                            let current = config.read().enabled;
                            config.write().enabled = !current;
                        },
                        style: "cursor: pointer; accent-color: var(--btn-primary); width: 16px; height: 16px;",
                    }
                }
            }

            // Preset
            SettingRow {
                label: "Encoding Preset",
                select {
                    style: select_style(),
                    value: "{config.read().preset}",
                    onchange: move |evt: Event<FormData>| config.write().preset = evt.value(),
                    for preset in PRESETS {
                        option { value: *preset, selected: config.read().preset == *preset, "{preset}" }
                    }
                }
            }

            // Max bitrate
            SettingRow {
                label: "Max Bitrate (Mbps)",
                input {
                    r#type: "number",
                    min: "1",
                    max: "100",
                    value: "{config.read().max_bitrate_mbps}",
                    onchange: move |evt: Event<FormData>| {
                        if let Ok(v) = evt.value().parse::<u32>() {
                            config.write().max_bitrate_mbps = v;
                        }
                    },
                    style: "{input_style()} width: 80px;",
                }
            }

            // Max resolution
            SettingRow {
                label: "Max Resolution",
                select {
                    style: select_style(),
                    value: "{config.read().max_height}",
                    onchange: move |evt: Event<FormData>| {
                        if let Ok(v) = evt.value().parse::<u32>() {
                            config.write().max_height = v;
                        }
                    },
                    for (height, label) in RESOLUTIONS {
                        option { value: "{height}", selected: config.read().max_height == *height, "{label}" }
                    }
                }
            }

            // Audio bitrate
            SettingRow {
                label: "Audio Bitrate",
                select {
                    style: select_style(),
                    value: "{config.read().audio_bitrate_kbps}",
                    onchange: move |evt: Event<FormData>| {
                        if let Ok(v) = evt.value().parse::<u32>() {
                            config.write().audio_bitrate_kbps = v;
                        }
                    },
                    for rate in AUDIO_BITRATES {
                        option { value: "{rate}", selected: config.read().audio_bitrate_kbps == *rate, "{rate}k" }
                    }
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

            if saved() {
                div {
                    style: "margin-top: 8px; font-size: 12px; color: var(--success); text-align: center;",
                    "Settings saved"
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

fn select_style() -> &'static str {
    "padding: 6px 8px; border-radius: 4px; border: 1px solid var(--border); background: var(--bg-secondary); color: var(--text); font-size: 13px; width: 100%;"
}

fn input_style() -> &'static str {
    "padding: 6px 8px; border-radius: 4px; border: 1px solid var(--border); background: var(--bg-secondary); color: var(--text); font-size: 13px;"
}
