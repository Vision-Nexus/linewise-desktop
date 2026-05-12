//! Lightweight toast overlay. One toast visible at a time, replaced
//! immediately by any newer `show_toast` call, and auto-dismissed by a
//! task scheduled inside `AppState::show_toast` itself (so it doesn't
//! depend on the overlay being mounted when the toast fires). The
//! overlay sits inside the app shell so it layers above the title bar
//! and every modal; `pointer-events: none` on the wrapper means the
//! toast itself never steals clicks.

use crate::state::{AppState, ToastKind};
use dioxus::prelude::*;

#[component]
pub fn ToastOverlay() -> Element {
    let app_state = use_context::<AppState>();
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
