use crate::components::switch::{Switch, SwitchThumb};
use crate::state::CoreServices;
use dioxus::prelude::*;
use lw_core::config::AppConfig;

#[component]
pub fn UploadSettingsPane() -> Element {
    let services = use_context::<CoreServices>();
    let mut auto_clean = use_signal(|| services.upload_engine.auto_clean());

    let on_toggle = {
        let engine = services.upload_engine.clone();
        move |v: bool| {
            // 1. Apply live so in-flight uploads honor the new setting.
            engine.set_auto_clean(v);
            // 2. Reflect in the UI.
            auto_clean.set(v);
            // 3. Persist so it survives restart. We reload the file each
            //    time rather than holding a long-lived copy so we don't
            //    clobber unrelated fields edited elsewhere.
            match AppConfig::load() {
                Ok(mut cfg) => {
                    cfg.upload.auto_clean = v;
                    if let Err(e) = cfg.save() {
                        tracing::error!("Failed to persist auto_clean: {e}");
                    }
                }
                Err(e) => tracing::error!("Failed to load config for auto_clean save: {e}"),
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
