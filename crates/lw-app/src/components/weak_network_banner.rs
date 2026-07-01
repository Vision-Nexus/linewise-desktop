//! Weak-network prompt banner.
//!
//! A non-modal row that appears when connectivity has stayed in the weak band
//! ([`NetworkHealth::is_weak`] — Weak or Offline) continuously for longer than
//! [`WEAK_GRACE`], nudging the user toward a better network or a configured
//! proxy. Modeled on `version_banner::VersionUpdateBanner`.
//!
//! Edge-triggered, not per-tick: the underlying `network_health` signal only
//! updates on a tier change (the engine debounces its probe), so entering the
//! weak band arms a single grace timer; recovering to Ok/Good hides the banner
//! and clears the one-shot dismiss so the next weak episode can prompt again.

use crate::state::AppState;
use dioxus::prelude::*;
use lw_core::upload::NetworkHealth;

/// How long connectivity must stay weak before the banner appears. Short enough
/// to be useful on a genuinely bad link, long enough that a brief dip (one slow
/// probe) never flashes the prompt.
const WEAK_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

#[component]
pub fn WeakNetworkBanner() -> Element {
    let mut app_state = use_context::<AppState>();
    // `show`: the grace period elapsed while still weak. `dismissed`: the user
    // clicked "本次不再提示" for the current weak episode. Both reset when the
    // link recovers, so a later episode can prompt again.
    let mut show = use_signal(|| false);
    let mut dismissed = use_signal(|| false);
    // Monotonic episode counter, bumped on every weak↔healthy edge. A grace
    // timer captures the epoch when armed and no-ops if a later edge has since
    // bumped it — so a stale timer from a prior weak episode can't reveal the
    // banner early during a later one (weak→ok→weak flapping).
    let mut epoch = use_signal(|| 0u64);

    // Current weak-band membership, derived from the latest reading. `None`
    // (no probe yet) counts as not-weak. `NetworkReading` is `Copy`, so read a
    // value out of the `Ref` rather than mapping the guard.
    let weak = (*app_state.network_health.read())
        .map(|r| r.health.is_weak())
        .unwrap_or(false);

    // Arm/disarm on the weak↔healthy edge. Every edge bumps `epoch`, retiring
    // any prior grace timer. Entering weak spawns a single grace timer that
    // reveals the banner iff still weak AND its epoch is still current on
    // expiry; recovering hides it and clears the dismiss.
    use_effect(use_reactive!(|weak| {
        // Read `epoch` with peek() (UNTRACKED). A tracked read (`epoch()`) here
        // would subscribe this effect to `epoch`, and the `epoch.set` below would
        // then re-trigger the effect endlessly — a main-thread busy loop that
        // freezes the UI. This effect must re-run only on `weak` (via use_reactive!).
        let my_epoch = (*epoch.peek()).wrapping_add(1);
        epoch.set(my_epoch);
        if !weak {
            show.set(false);
            dismissed.set(false);
            return;
        }
        spawn(async move {
            tokio::time::sleep(WEAK_GRACE).await;
            // Untracked read — this runs in a spawned future, and peek() also
            // keeps it from ever subscribing anything.
            if *epoch.peek() != my_epoch {
                return;
            }
            let still_weak = (*app_state.network_health.read())
                .map(|r| r.health.is_weak())
                .unwrap_or(false);
            if still_weak {
                show.set(true);
            }
        });
    }));

    if !*show.read() || *dismissed.read() {
        return rsx! {};
    }

    // Offline gets the red palette; a merely-weak link the warning palette.
    let offline = matches!(
        (*app_state.network_health.read()).map(|r| r.health),
        Some(NetworkHealth::Offline)
    );
    let (bg, border) = if offline {
        ("var(--error-bg)", "var(--error)")
    } else {
        ("var(--warning-bg)", "var(--warning)")
    };

    rsx! {
        div {
            style: "display: flex; align-items: center; gap: 12px; \
                    padding: 8px 16px; \
                    background: {bg}; border-bottom: 1px solid {border}; \
                    color: var(--text); font-size: 13px;",
            span {
                style: "flex: 1;",
                "网络太弱,无法连接存储。请更换更稳定的网络,或在 设置 → 网络 中配置代理。"
            }
            button {
                class: "btn-primary",
                style: "padding: 6px 12px; border-radius: 6px; \
                        background: var(--btn-primary); color: white; \
                        border: none; cursor: pointer; font-size: 12px; font-weight: 600;",
                onclick: move |_| app_state.show_settings.set(true),
                "打开设置"
            }
            button {
                style: "padding: 6px 12px; border-radius: 6px; \
                        background: transparent; color: var(--text-secondary); \
                        border: 1px solid var(--border); cursor: pointer; font-size: 12px;",
                onclick: move |_| dismissed.set(true),
                "本次不再提示"
            }
        }
    }
}
