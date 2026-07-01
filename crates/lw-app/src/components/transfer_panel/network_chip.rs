//! Signal-strength chip for the transfer-panel header.
//!
//! A "game ping"-style indicator: four stacked bars whose lit count and colour
//! track the latest [`NetworkReading`] the engine's probe reported (via
//! `AppState::network_health`). It renders nothing until the first reading
//! lands, so a freshly-launched app doesn't flash a bogus tier.

use crate::state::AppState;
use dioxus::prelude::*;
use lw_core::upload::{NetworkHealth, NetworkReading};

/// Display attributes derived from a health tier: how many of the four bars are
/// lit, their colour (an existing CSS var), and an optional trailing label. The
/// two healthy tiers show bars only; the two unhealthy tiers add a short label.
struct ChipStyle {
    lit_bars: u8,
    color: &'static str,
    label: &'static str,
}

/// Pure map from a health tier to its chip attributes. Kept separate from the
/// component so the tier→style mapping is exhaustive (the compiler flags a new
/// [`NetworkHealth`] variant here) and trivially reviewable.
fn chip_style(health: NetworkHealth) -> ChipStyle {
    match health {
        NetworkHealth::Good => ChipStyle {
            lit_bars: 4,
            color: "var(--success)",
            label: "",
        },
        NetworkHealth::Ok => ChipStyle {
            lit_bars: 3,
            color: "var(--warning)",
            label: "",
        },
        NetworkHealth::Weak => ChipStyle {
            lit_bars: 2,
            color: "var(--warning)",
            label: "Weak",
        },
        NetworkHealth::Offline => ChipStyle {
            lit_bars: 1,
            color: "var(--error)",
            label: "Offline",
        },
    }
}

/// Hover-title text. When parts are actively retrying we say so — the probe RTT
/// would be misleading (a green probe alongside failing part PUTs is exactly the
/// false-green case this chip corrects). Otherwise: the probe RTT when known,
/// else a plain tier note. Ok/Weak use the orange `--warning`; only Offline is
/// red, so "Weak" reads as distinct from "Offline".
fn hover_title(reading: NetworkReading, retrying: bool) -> String {
    if retrying {
        return "Uploads retrying — weak connection".to_string();
    }
    match reading.rtt_ms {
        Some(ms) => format!("ping {ms}ms"),
        None => "Unreachable".to_string(),
    }
}

/// Four-bar signal chip. Reads `network_health`; renders nothing while it is
/// `None`. One bar element per slot: lit slots take the tier colour, unlit slots
/// a muted track colour, with ascending heights for the classic signal look.
///
/// The displayed tier is the WORSE of the probe reading and — when any upload
/// part is currently retrying (`part_retrying` non-empty) — a `Weak` floor. So a
/// green probe alongside failing part PUTs renders orange "Weak" instead of a
/// false "Good". Purely derived from the two signals it reads; no self-writing
/// effect (avoids the tracked-read-then-write footgun).
#[component]
pub fn NetworkChip() -> Element {
    let app_state = use_context::<AppState>();
    let Some(reading) = *app_state.network_health.read() else {
        return rsx! {};
    };
    // Any part in the retry map means part PUTs are actively failing/backing off.
    let retrying = !app_state.part_retrying.read().is_empty();
    let health = if retrying {
        reading.health.at_least_weak()
    } else {
        reading.health
    };
    let style = chip_style(health);
    let title = hover_title(reading, retrying);
    // Ascending bar heights (px) for slots 1..=4.
    let heights: [u8; 4] = [6, 9, 12, 15];

    rsx! {
        div {
            title: "{title}",
            style: "display: inline-flex; align-items: center; gap: 6px; \
                    padding: 2px 8px; border-radius: 12px; \
                    background: var(--bg-secondary); border: 1px solid var(--border);",
            div {
                style: "display: inline-flex; align-items: flex-end; gap: 2px; height: 15px;",
                for (idx , h) in heights.iter().enumerate() {
                    {
                        let lit = (idx as u8) < style.lit_bars;
                        let bar_color = if lit { style.color } else { "var(--border)" };
                        rsx! {
                            div {
                                key: "{idx}",
                                style: "width: 3px; height: {h}px; border-radius: 1px; background: {bar_color};",
                            }
                        }
                    }
                }
            }
            if !style.label.is_empty() {
                span {
                    style: "font-size: 11px; font-weight: 600; color: {style.color};",
                    "{style.label}"
                }
            }
        }
    }
}
