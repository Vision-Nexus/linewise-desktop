use crate::components::switch::{Switch, SwitchThumb};
use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;

#[component]
pub fn UploadSettingsPane() -> Element {
    let services = use_context::<CoreServices>();
    let mut app_state = use_context::<AppState>();
    let mut auto_clean = use_signal(|| services.upload_engine.auto_clean());

    let on_toggle = {
        let engine = services.upload_engine.clone();
        move |v: bool| {
            // 1. Apply live so in-flight uploads honor the new setting.
            engine.set_auto_clean(v);
            // 2. Reflect in the UI.
            auto_clean.set(v);
            // 3. Persist via the live config signal so other readers
            //    pick up the change without a restart.
            let mut next = app_state.config.read().clone();
            next.upload.auto_clean = v;
            if let Err(e) = app_state.save_config(next) {
                tracing::error!("Failed to persist auto_clean: {e}");
            }
        }
    };

    rsx! {
        div {
            style: "background: var(--bg); color: var(--text);",

            // Auto-clean toggle — live-applied to the running UploadEngine.
            div {
                style: "display: flex; align-items: flex-start; gap: 12px;",
                div {
                    style: "flex: 1;",
                    div { style: "font-size: 13px; font-weight: 500;", "Auto-clean local files" }
                    div {
                        style: "font-size: 12px; color: var(--text-secondary); margin-top: 2px;",
                        "Delete the original file from disk after a successful upload."
                    }
                }
                Switch {
                    checked: auto_clean(),
                    aria_label: "Auto-clean local files after successful upload",
                    on_checked_change: on_toggle,
                    SwitchThumb {}
                }
            }
        }
    }
}
