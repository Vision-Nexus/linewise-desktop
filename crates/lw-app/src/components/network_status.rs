//! Network status pill for the sidebar footer.
//!
//! Maps the engine's real 4-tier [`NetworkHealth`] probe to the wave prototype's
//! three user-facing states (Good / Slow / Offline) and folds the old
//! weak-network banner's guidance into a click-through popover (including the
//! "Open Settings" action for a bad link). Replaces the transfer-panel bar chip.
//!
//! The displayed tier is the WORSE of the probe reading and — when any upload
//! part is currently retrying — a `Weak` floor, so a green probe alongside
//! failing part PUTs still reads as "Slow" (the same false-green correction the
//! old chip made). Purely derived from the signals it reads; no self-writing
//! effect.

use crate::state::AppState;
use dioxus::prelude::*;
use lw_core::upload::NetworkHealth;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PillStatus {
    Connected,
    Degraded,
    Offline,
}

const ORDER: [PillStatus; 3] = [
    PillStatus::Connected,
    PillStatus::Degraded,
    PillStatus::Offline,
];

impl PillStatus {
    /// 4-tier engine health → 3-tier user status (D3): Good→Connected,
    /// Ok/Weak→Degraded ("Slow"), Offline→Offline.
    fn from_health(h: NetworkHealth) -> Self {
        match h {
            NetworkHealth::Good => Self::Connected,
            NetworkHealth::Ok | NetworkHealth::Weak => Self::Degraded,
            NetworkHealth::Offline => Self::Offline,
        }
    }

    fn short(self) -> &'static str {
        match self {
            Self::Connected => "Good",
            Self::Degraded => "Slow",
            Self::Offline => "Offline",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Degraded => "Slow connection",
            Self::Offline => "Offline",
        }
    }

    fn headline(self) -> &'static str {
        match self {
            Self::Connected => "Connection stable",
            Self::Degraded => "Connection is slow or unstable",
            Self::Offline => "No internet connection",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Connected => "Uploads and quality checks are proceeding normally.",
            Self::Degraded => {
                "Uploads may pause, retry, or take longer until your network improves."
            }
            Self::Offline => "Uploads and quality checks are paused until you're back online.",
        }
    }

    fn callout(self) -> &'static str {
        match self {
            Self::Connected => {
                "Slow uploads are usually due to file size, not a connection problem."
            }
            Self::Degraded => "Uploads will resume automatically when your connection improves.",
            Self::Offline => "Uploads will resume when you reconnect.",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::Connected => "var(--success)",
            Self::Degraded => "var(--warning)",
            Self::Offline => "var(--error)",
        }
    }

    fn pill_bg(self) -> &'static str {
        match self {
            Self::Connected => "rgba(34,197,94,0.10)",
            Self::Degraded => "rgba(245,158,11,0.10)",
            Self::Offline => "rgba(239,68,68,0.10)",
        }
    }

    fn pill_border(self) -> &'static str {
        match self {
            Self::Connected => "rgba(34,197,94,0.28)",
            Self::Degraded => "rgba(245,158,11,0.32)",
            Self::Offline => "rgba(239,68,68,0.32)",
        }
    }
}

/// The sidebar-footer network status pill. Renders nothing until the first probe
/// lands (so a fresh launch doesn't flash a bogus tier).
#[component]
pub fn NetworkStatusPill() -> Element {
    let app_state = use_context::<AppState>();
    let mut show_settings = app_state.show_settings;
    let mut open = use_signal(|| false);

    let Some(reading) = *app_state.network_health.read() else {
        return rsx! {};
    };
    // Floor to at-least-weak while parts are actively retrying — a green probe
    // alongside failing PUTs is exactly the false-green case to correct.
    let retrying = !app_state.part_retrying.read().is_empty();
    let health = if retrying {
        reading.health.at_least_weak()
    } else {
        reading.health
    };
    let status = PillStatus::from_health(health);
    let is_offline = status == PillStatus::Offline;
    let needs_help = status != PillStatus::Connected;

    rsx! {
        div {
            style: "position: relative; padding: 8px; border-top: 1px solid var(--border);",
            button {
                style: "display: inline-flex; align-items: center; gap: 6px; height: 28px; width: 100%; \
                        padding: 0 8px; border-radius: 6px; cursor: pointer; font-size: 11px; \
                        font-weight: 500; color: {status.color()}; background: {status.pill_bg()}; \
                        border: 1px solid {status.pill_border()};",
                "aria-label": "Network status: {status.label()}",
                onclick: move |_| {
                    let next = !*open.read();
                    open.set(next);
                },
                span {
                    style: "position: relative; display: inline-flex; width: 8px; height: 8px; flex-shrink: 0;",
                    if status == PillStatus::Degraded {
                        span {
                            class: "lw-ping",
                            style: "position: absolute; inset: 0; border-radius: 999px; background: {status.color()};",
                        }
                    }
                    span {
                        style: "position: relative; width: 8px; height: 8px; border-radius: 999px; background: {status.color()};",
                    }
                }
                if is_offline {
                    crate::icons::WifiOffIcon {}
                } else {
                    crate::icons::WifiIcon {}
                }
                span {
                    style: "overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                    "{status.short()}"
                }
            }

            if *open.read() {
                div {
                    style: "position: fixed; inset: 0; z-index: 40;",
                    onclick: move |_| open.set(false),
                }
                div {
                    style: "position: absolute; bottom: 100%; left: 8px; right: 8px; margin-bottom: 6px; \
                            z-index: 50; background: var(--bg); border: 1px solid var(--border); \
                            border-radius: 8px; box-shadow: 0 4px 16px rgba(0,0,0,0.18); padding: 8px; \
                            max-height: 60vh; overflow-y: auto;",
                    div {
                        style: "font-size: 11px; color: var(--text-muted); padding: 2px 4px 6px;",
                        "Network status"
                    }
                    for st in ORDER.iter() {
                        {
                            let st = *st;
                            let active = st == status;
                            let card_border = if active { "var(--border-hover)" } else { "var(--border)" };
                            let card_bg = if active { "var(--bg-secondary)" } else { "transparent" };
                            rsx! {
                                div {
                                    key: "{st.short()}",
                                    style: "border: 1px solid {card_border}; background: {card_bg}; \
                                            border-radius: 6px; padding: 8px; margin-bottom: 6px;",
                                    div {
                                        style: "display: flex; align-items: center; gap: 6px; margin-bottom: 3px;",
                                        span { style: "width: 8px; height: 8px; border-radius: 999px; background: {st.color()};" }
                                        span { style: "font-size: 11px; font-weight: 500; color: var(--text);", "{st.label()}" }
                                        if active {
                                            span { style: "font-size: 10px; color: var(--text-muted);", "(active)" }
                                        }
                                    }
                                    div { style: "font-size: 11px; font-weight: 500; color: var(--text);", "{st.headline()}" }
                                    div { style: "font-size: 11px; color: var(--text-muted); margin-top: 2px; line-height: 1.35;", "{st.description()}" }
                                    div {
                                        style: "margin-top: 6px; border: 1px solid var(--border); background: var(--bg-tertiary); \
                                                border-radius: 6px; padding: 6px 8px;",
                                        div { style: "font-size: 10px; font-weight: 600; color: var(--text);", "Keep the app open" }
                                        div { style: "font-size: 10px; color: var(--text-muted); margin-top: 1px; line-height: 1.35;", "{st.callout()}" }
                                    }
                                }
                            }
                        }
                    }
                    if needs_help {
                        button {
                            style: "width: 100%; height: 28px; border-radius: 6px; cursor: pointer; \
                                    background: var(--btn-primary); color: white; border: none; \
                                    font-size: 12px; font-weight: 500; margin-top: 2px;",
                            onclick: move |_| {
                                open.set(false);
                                show_settings.set(true);
                            },
                            "Open Settings"
                        }
                    }
                }
            }
        }
    }
}
