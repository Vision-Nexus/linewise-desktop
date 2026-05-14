//! Lightweight toast overlay. One toast visible at a time, replaced
//! immediately by any newer `show_toast` call. Auto-dismiss lives here
//! rather than inside `show_toast` so it survives subtree remounts
//! triggered by the environment switcher — the overlay is mounted at
//! the root and its scope outlives every caller. The overlay sits
//! inside the app shell so it layers above the title bar and every
//! modal; `pointer-events: none` on the wrapper means the toast itself
//! never steals clicks.

use crate::state::{AppState, ToastKind};
use dioxus::prelude::*;

const TOAST_LIFETIME_MS: u64 = 2500;

#[component]
pub fn ToastOverlay() -> Element {
    let mut app_state = use_context::<AppState>();

    // Re-arm the dismiss timer every time a new toast id appears. The
    // id guard inside the task means an older toast's timer can't
    // accidentally clear a newer replacement.
    let current_id = app_state.toast.read().as_ref().map(|t| t.id);
    use_effect(use_reactive!(|current_id| {
        let Some(id) = current_id else { return };
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(TOAST_LIFETIME_MS)).await;
            let still_mine = app_state
                .toast
                .read()
                .as_ref()
                .map(|t| t.id == id)
                .unwrap_or(false);
            if still_mine {
                app_state.toast.set(None);
            }
        });
    }));

    let Some(toast) = app_state.toast.read().clone() else {
        return rsx! {};
    };

    let (bg, border, fg) = match toast.kind {
        ToastKind::Success => ("var(--success-bg)", "var(--success)", "var(--success)"),
        ToastKind::Error => ("var(--error-bg)", "var(--error)", "var(--error)"),
        ToastKind::Info => ("var(--info-bg)", "var(--info)", "var(--info)"),
    };

    let icon = match toast.kind {
        ToastKind::Success => "✓",
        ToastKind::Error => "!",
        ToastKind::Info => "i",
    };

    rsx! {
        div {
            style: "position: fixed; top: 52px; left: 0; right: 0; \
                    display: flex; justify-content: center; \
                    pointer-events: none; z-index: 2000;",
            div {
                class: "slide-down",
                style: "pointer-events: auto; display: flex; align-items: center; gap: 10px; \
                        padding: 10px 16px; border-radius: 8px; \
                        background: {bg}; border: 1px solid {border}; color: {fg}; \
                        box-shadow: 0 6px 20px rgba(0,0,0,0.18); \
                        font-size: 13px; font-weight: 500; \
                        min-width: 240px; max-width: 520px;",
                div {
                    style: "width: 20px; height: 20px; border-radius: 9999px; \
                            background: {fg}; color: white; \
                            display: flex; align-items: center; justify-content: center; \
                            font-size: 12px; font-weight: 700; flex-shrink: 0;",
                    "{icon}"
                }
                span { style: "color: var(--text);", "{toast.message}" }
            }
        }
    }
}
