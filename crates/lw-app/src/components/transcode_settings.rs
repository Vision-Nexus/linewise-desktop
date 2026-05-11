use crate::components::select::{Select, SelectList, SelectOption, SelectTrigger, SelectValue};
use crate::components::switch::{Switch, SwitchThumb};
use dioxus::prelude::*;
use lw_core::config::{AppConfig, TranscodeConfig};
use lw_core::transcode::probe_availability;

const PRESETS: &[&str] = &["fast", "medium", "slow"];
const RESOLUTIONS: &[(u32, &str)] = &[(720, "720p"), (1080, "1080p")];
const AUDIO_BITRATES: &[u32] = &[128, 192];

#[component]
pub fn TranscodeSettingsPane() -> Element {
    let mut config = use_signal(|| AppConfig::load().map(|c| c.transcode).unwrap_or_default());
    let mut saved = use_signal(|| false);
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

            // Hardware acceleration — sub-setting of the master toggle. Filtered
            // to Auto + None + whatever HW encoders the current ffmpeg build has.
            SettingRow {
                label: "Hardware Acceleration",
                {
                    let hw_options: Vec<(String, String)> = {
                        let avail = availability.read();
                        std::iter::once(("auto".to_string(), "Auto (prefer HW if available)".to_string()))
                            .chain(std::iter::once(("none".to_string(), "None (software)".to_string())))
                            .chain(avail.available_hw.iter().map(|hw| (hw.as_config_str().to_string(), hw.display_label().to_string())))
                            .collect()
                    };
                    let current = config.read().hw_accel.clone();
                    rsx! {
                        Select::<String> {
                            key: "{current}",
                            default_value: current.clone(),
                            disabled: !master_enabled,
                            on_value_change: move |v: Option<String>| {
                                if let Some(v) = v {
                                    config.write().hw_accel = v;
                                }
                            },
                            SelectTrigger { aria_label: "Hardware acceleration",
                                SelectValue { placeholder: "Select..." }
                            }
                            SelectList { aria_label: "Hardware acceleration options",
                                for (i, (value, label)) in hw_options.iter().enumerate() {
                                    SelectOption::<String> {
                                        index: i,
                                        value: value.clone(),
                                        text_value: label.clone(),
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Preset
            SettingRow {
                label: "Encoding Preset",
                Select::<String> {
                    key: "{config.read().preset}",
                    default_value: config.read().preset.clone(),
                    disabled: !master_enabled,
                    on_value_change: move |v: Option<String>| {
                        if let Some(v) = v {
                            config.write().preset = v;
                        }
                    },
                    SelectTrigger { aria_label: "Encoding preset",
                        SelectValue { placeholder: "Select..." }
                    }
                    SelectList { aria_label: "Encoding preset options",
                        for (i, preset) in PRESETS.iter().enumerate() {
                            SelectOption::<String> {
                                index: i,
                                value: preset.to_string(),
                                text_value: preset.to_string(),
                                "{preset}"
                            }
                        }
                    }
                }
            }

            // Target average bitrate (the VBR target).
            SettingRow {
                label: "Target Bitrate (Mbps)",
                input {
                    r#type: "number",
                    min: "1",
                    max: "100",
                    value: "{config.read().target_bitrate_mbps}",
                    disabled: !master_enabled,
                    onchange: move |evt: Event<FormData>| {
                        if let Ok(v) = evt.value().parse::<u32>() {
                            config.write().target_bitrate_mbps = v;
                        }
                    },
                    style: "{input_style()} width: 80px;",
                }
            }

            // Peak cap. Typical 2× target; at equal values VideoToolbox
            // systematically undershoots the target.
            SettingRow {
                label: "Peak Bitrate Cap (Mbps)",
                input {
                    r#type: "number",
                    min: "1",
                    max: "100",
                    value: "{config.read().max_bitrate_mbps}",
                    disabled: !master_enabled,
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
                Select::<u32> {
                    key: "{config.read().max_height}",
                    default_value: config.read().max_height,
                    disabled: !master_enabled,
                    on_value_change: move |v: Option<u32>| {
                        if let Some(v) = v {
                            config.write().max_height = v;
                        }
                    },
                    SelectTrigger { aria_label: "Max resolution",
                        SelectValue { placeholder: "Select..." }
                    }
                    SelectList { aria_label: "Max resolution options",
                        for (i, (height, label)) in RESOLUTIONS.iter().enumerate() {
                            SelectOption::<u32> {
                                index: i,
                                value: *height,
                                text_value: label.to_string(),
                                "{label}"
                            }
                        }
                    }
                }
            }

            // Audio bitrate
            SettingRow {
                label: "Audio Bitrate",
                Select::<u32> {
                    key: "{config.read().audio_bitrate_kbps}",
                    default_value: config.read().audio_bitrate_kbps,
                    disabled: !master_enabled,
                    on_value_change: move |v: Option<u32>| {
                        if let Some(v) = v {
                            config.write().audio_bitrate_kbps = v;
                        }
                    },
                    SelectTrigger { aria_label: "Audio bitrate",
                        SelectValue { placeholder: "Select..." }
                    }
                    SelectList { aria_label: "Audio bitrate options",
                        for (i, rate) in AUDIO_BITRATES.iter().enumerate() {
                            SelectOption::<u32> {
                                index: i,
                                value: *rate,
                                text_value: format!("{rate}k"),
                                "{rate}k"
                            }
                        }
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

fn input_style() -> &'static str {
    "padding: 6px 8px; border-radius: 4px; border: 1px solid var(--border); background: var(--bg-secondary); color: var(--text); font-size: 13px;"
}
