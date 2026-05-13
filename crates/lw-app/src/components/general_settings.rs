use crate::state::{AppState, ToastKind};
use dioxus::prelude::*;
use lw_core::config::AppConfig;
use lw_core::logging::DEFAULT_LOG_FILTER;

/// General app settings — currently only the tracing log filter, which
/// drives both the stdout fmt layer and the rolling file appender. The
/// value is a tracing-subscriber EnvFilter directive (e.g.
/// `info,lw_app=trace`); it takes effect on next launch since the
/// subscriber is initialised once at startup.
#[component]
pub fn GeneralSettingsPane() -> Element {
    let mut app_state = use_context::<AppState>();
    let initial = AppConfig::load()
        .map(|c| c.app.log_filter)
        .unwrap_or_else(|_| DEFAULT_LOG_FILTER.to_string());
    let mut log_filter = use_signal(|| initial);

    let save = move |_| {
        let value = log_filter.read().clone();
        match AppConfig::load() {
            Ok(mut cfg) => {
                cfg.app.log_filter = value;
                match cfg.save() {
                    Ok(()) => {
                        app_state.show_toast(
                            "Log filter saved — takes effect on next launch",
                            ToastKind::Success,
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to save log_filter: {e}");
                        app_state
                            .show_toast(format!("Failed to save settings: {e}"), ToastKind::Error);
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to load config for log_filter save: {e}");
                app_state.show_toast(
                    format!("Failed to load config for save: {e}"),
                    ToastKind::Error,
                );
            }
        }
    };

    let reset = move |_| {
        log_filter.set(DEFAULT_LOG_FILTER.to_string());
    };

    rsx! {
        div {
            style: "background: var(--bg); color: var(--text);",

            label {
                style: "display: block; font-size: 13px; font-weight: 500; margin-bottom: 4px;",
                "Log filter"
            }
            div {
                style: "font-size: 12px; color: var(--text-secondary); margin-bottom: 6px;",
                "tracing EnvFilter directive — e.g. ",
                code { style: "font-family: ui-monospace, SFMono-Regular, Menlo, monospace;",
                    "info,lw_app=trace"
                },
                ". Takes effect on next launch."
            }
            input {
                r#type: "text",
                value: "{log_filter}",
                spellcheck: "false",
                autocapitalize: "off",
                autocorrect: "off",
                oninput: move |e: Event<FormData>| log_filter.set(e.value()),
                style: "width: 100%; padding: 8px 10px; border-radius: 6px; \
                        border: 1px solid var(--border); background: var(--bg-secondary); \
                        color: var(--text); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; \
                        font-size: 12px; box-sizing: border-box;",
            }

            div {
                style: "display: flex; gap: 8px; margin-top: 12px;",
                button {
                    style: "flex: 1; padding: 8px 16px; border-radius: 6px; border: none; \
                            background: var(--btn-primary); color: white; cursor: pointer; \
                            font-weight: 500; font-size: 13px;",
                    onclick: save,
                    "Save"
                }
                button {
                    style: "padding: 8px 16px; border-radius: 6px; border: 1px solid var(--border); \
                            background: transparent; color: var(--text-secondary); cursor: pointer; \
                            font-size: 13px;",
                    onclick: reset,
                    "Reset"
                }
            }
        }
    }
}
