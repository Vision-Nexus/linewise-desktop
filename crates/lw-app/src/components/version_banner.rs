//! UI surfaces for the startup version check.
//!
//! Two components:
//! - [`VersionUpdateBanner`] — slim row that appears under the title bar
//!   when a newer release exists. Non-modal, dismiss-on-update.
//! - [`VersionBlockedScreen`] — full-screen card that replaces the app
//!   shell when the running version is below `minSupported`. Modeled on
//!   `DbErrorScreen` so the recovery-style framing is consistent.

use crate::state::AppState;
use dioxus::prelude::*;
use lw_core::version_check::VersionStatus;

/// `webbrowser::open` is synchronous and on Linux can briefly stall the
/// renderer while `xdg-open` hands off. Spawn the call onto a blocking task
/// so the click handler returns immediately.
pub fn open_release_page(url: String) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = webbrowser::open(&url) {
            tracing::warn!("Failed to open release page {url}: {e}");
        }
    });
}

#[component]
pub fn VersionUpdateBanner() -> Element {
    let app_state = use_context::<AppState>();
    let status = app_state.version_status.read().clone();

    let Some(VersionStatus::UpdateAvailable {
        latest,
        release_url,
        ..
    }) = status
    else {
        return rsx! {};
    };

    rsx! {
        div {
            style: "display: flex; align-items: center; gap: 12px; \
                    padding: 8px 16px; \
                    background: var(--info-bg); border-bottom: 1px solid var(--info); \
                    color: var(--text); font-size: 13px;",
            div {
                style: "width: 18px; height: 18px; border-radius: 9999px; \
                        background: var(--info); color: white; \
                        display: flex; align-items: center; justify-content: center; \
                        font-size: 12px; font-weight: 600; flex-shrink: 0;",
                "i"
            }
            span {
                style: "flex: 1;",
                "A new version (v{latest}) of Linewise Desktop is available."
            }
            button {
                class: "btn-primary",
                style: "padding: 6px 12px; border-radius: 6px; \
                        background: var(--btn-primary); color: white; \
                        border: none; cursor: pointer; font-size: 12px; font-weight: 600;",
                onclick: move |_| open_release_page(release_url.clone()),
                "Open release page"
            }
        }
    }
}

#[component]
pub fn VersionBlockedScreen() -> Element {
    let app_state = use_context::<AppState>();
    let status = app_state.version_status.read().clone();

    let Some(VersionStatus::Unsupported {
        running,
        min_supported,
        latest,
        release_url,
    }) = status
    else {
        return rsx! {};
    };

    rsx! {
        div {
            class: "h-full w-full flex items-center justify-center p-8",
            div {
                class: "max-w-[520px] w-full flex flex-col gap-4 bg-background border border-border rounded-lg p-6 shadow-md",

                h2 {
                    class: "text-lg font-semibold text-foreground",
                    "Update required"
                }
                p {
                    class: "text-sm text-muted-foreground",
                    "This version of Linewise Desktop is no longer supported. \
                     Please update to continue."
                }
                div {
                    class: "text-xs bg-muted text-foreground border border-border rounded px-3 py-2",
                    div { "Running: v{running}" }
                    div { "Minimum supported: v{min_supported}" }
                    div { "Latest available: v{latest}" }
                }
                div {
                    class: "flex gap-2 justify-end border-t border-border pt-4",
                    button {
                        class: "h-9 px-4 text-sm rounded bg-primary text-primary-foreground hover:opacity-90 cursor-pointer",
                        onclick: move |_| open_release_page(release_url.clone()),
                        "Open release page"
                    }
                }
            }
        }
    }
}
