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
            label: "网络较差",
        },
        NetworkHealth::Offline => ChipStyle {
            lit_bars: 1,
            color: "var(--error)",
            label: "已离线",
        },
    }
}

/// Hover-title text: the probe RTT when known, else a plain tier note. Ok/Weak
/// use the orange `--warning`; only Offline is red, so "网络较差" reads as
/// distinct from "已离线".
fn hover_title(reading: NetworkReading) -> String {
    match reading.rtt_ms {
        Some(ms) => format!("ping {ms}ms"),
        None => "网络不可达".to_string(),
    }
}

/// Four-bar signal chip. Reads `network_health`; renders nothing while it is
/// `None`. One bar element per slot: lit slots take the tier colour, unlit slots
/// a muted track colour, with ascending heights for the classic signal look.
#[component]
pub fn NetworkChip() -> Element {
    let app_state = use_context::<AppState>();
    let Some(reading) = *app_state.network_health.read() else {
        return rsx! {};
    };
    let style = chip_style(reading.health);
    let title = hover_title(reading);
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
