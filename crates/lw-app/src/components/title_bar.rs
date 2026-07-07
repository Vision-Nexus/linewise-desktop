//! Custom title bar for the Dioxus desktop window.
//!
//! macOS uses the native transparent-titlebar + fullsize-content-view
//! pattern (traffic lights stay in place); Windows and Linux are frameless
//! with the min/max/close buttons rendered inside this bar. The bar owns
//! drag, double-click-to-maximize and the window controls. The logo,
//! network status, settings, repair and account menu all live in the
//! sidebar now (brand header + profile footer); only a pre-login Repair
//! affordance remains here, since the sidebar doesn't exist until sign-in.
//!
//! Drag handling: the root `<div>` starts a native window drag on
//! `onmousedown`. Every interactive child (buttons, popover roots) must
//! call `e.stop_propagation()` on its own `onmousedown` — otherwise
//! clicking a control would start a drag instead of firing `onclick`. The
//! flex-1 spacer intentionally lets mousedown bubble so the empty middle
//! region acts as a drag handle.

use crate::state::AppState;
use dioxus::prelude::*;

fn begin_drag() {
    dioxus::desktop::window().drag();
}

fn minimize_window() {
    dioxus::desktop::window().set_minimized(true);
}

fn toggle_maximize() {
    let w = dioxus::desktop::window();
    let is_max = w.is_maximized();
    w.set_maximized(!is_max);
}

fn hide_to_tray() {
    dioxus::desktop::window().set_visible(false);
}

#[component]
pub fn TitleBar() -> Element {
    let app_state = use_context::<AppState>();
    let is_authenticated = *app_state.is_authenticated.read();

    // macOS keeps native traffic lights in the top-left; reserve room for
    // them and skip rendering our own min/max/close. Windows/Linux go fully
    // frameless, so we own the chrome.
    let logo_padding = if cfg!(target_os = "macos") {
        "pl-[78px] pr-2"
    } else {
        "pl-3 pr-2"
    };
    let show_custom_controls = !cfg!(target_os = "macos");

    rsx! {
        div {
            class: "h-9 flex items-center select-none bg-background border-b border-border shrink-0",
            onmousedown: move |_| begin_drag(),
            ondoubleclick: move |_| toggle_maximize(),

            // Left cluster — reserves macOS traffic-light room. The logo and the
            // account/settings menu now live in the sidebar (brand header +
            // profile footer); only the pre-login Repair affordance stays here,
            // because the sidebar doesn't exist until after sign-in and a wedged
            // app still needs Repair reachable.
            div {
                class: "{logo_padding} flex items-center gap-1 shrink-0",
                if !is_authenticated {
                    RepairButton {}
                }
            }

            // Drag spacer — flex-1 so it fills the middle. Intentionally
            // does NOT stop propagation so the empty region drags the window.
            div { class: "flex-1 h-full" }

            if show_custom_controls {
                WindowControls {}
            }
        }
    }
}

#[component]
fn RepairButton() -> Element {
    let mut app_state = use_context::<AppState>();
    rsx! {
        button {
            class: "w-7 h-7 flex items-center justify-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive appearance-none border-none bg-transparent cursor-pointer",
            title: "Repair — reset local app data",
            aria_label: "Open repair",
            onmousedown: move |e| e.stop_propagation(),
            onclick: move |_| app_state.show_repair.set(true),
            crate::icons::WrenchIcon {}
        }
    }
}

#[component]
fn WindowControls() -> Element {
    let mut is_maximized = use_signal(|| dioxus::desktop::window().is_maximized());

    rsx! {
        button {
            class: "w-12 h-9 flex items-center justify-center hover:bg-accent text-muted-foreground appearance-none border-none bg-transparent cursor-pointer",
            title: "Minimize",
            aria_label: "Minimize",
            onmousedown: move |e| e.stop_propagation(),
            onclick: move |_| minimize_window(),
            crate::icons::MinimizeIcon {}
        }
        button {
            class: "w-12 h-9 flex items-center justify-center hover:bg-accent text-muted-foreground appearance-none border-none bg-transparent cursor-pointer",
            title: if *is_maximized.read() { "Restore" } else { "Maximize" },
            aria_label: "Toggle maximize",
            onmousedown: move |e| e.stop_propagation(),
            onclick: move |_| {
                toggle_maximize();
                let w = dioxus::desktop::window();
                is_maximized.set(w.is_maximized());
            },
            if *is_maximized.read() {
                crate::icons::RestoreIcon {}
            } else {
                crate::icons::MaximizeIcon {}
            }
        }
        button {
            class: "w-12 h-9 flex items-center justify-center hover:bg-destructive hover:text-white text-muted-foreground appearance-none border-none bg-transparent cursor-pointer",
            title: "Close",
            aria_label: "Close",
            onmousedown: move |e| e.stop_propagation(),
            onclick: move |_| hide_to_tray(),
            crate::icons::CloseIcon {}
        }
    }
}
