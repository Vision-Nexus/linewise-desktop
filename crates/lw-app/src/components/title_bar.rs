//! Custom title bar for the Dioxus desktop window.
//!
//! macOS uses the native transparent-titlebar + fullsize-content-view
//! pattern (traffic lights stay in place); Windows and Linux are frameless
//! with the min/max/close buttons rendered inside this bar. Either way,
//! the bar owns drag, double-click-to-maximize, the app logo, the settings
//! gear, and the user avatar menu. Tenant and project selection live in
//! the sidebar because those lists can grow large.
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

            // Left cluster — logo plus Repair. Repair sits next to the
            // logo (rather than in the right cluster) so it stays
            // reachable independent of auth: a wedged app needs this
            // button before sign-in completes, and grouping it with the
            // user/settings cluster would imply otherwise.
            div {
                class: "{logo_padding} flex items-center gap-1 shrink-0",
                crate::icons::LinewiseLogo { width: "96" }
                RepairButton {}
            }

            // Drag spacer — flex-1 so it fills the middle. Intentionally
            // does NOT stop propagation so the empty region drags the window.
            div { class: "flex-1 h-full" }

            // Right cluster — settings + user menu (auth-gated).
            if is_authenticated {
                SettingsButton {}
                UserMenuButton {}
            }

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
fn SettingsButton() -> Element {
    let mut app_state = use_context::<AppState>();
    rsx! {
        button {
            class: "w-9 h-9 flex items-center justify-center hover:bg-accent text-muted-foreground appearance-none border-none bg-transparent cursor-pointer",
            title: "Settings",
            aria_label: "Open settings",
            onmousedown: move |e| e.stop_propagation(),
            onclick: move |_| app_state.show_settings.set(true),
            crate::icons::SettingsIcon {}
        }
    }
}

#[component]
fn UserMenuButton() -> Element {
    let app_state = use_context::<AppState>();
    let mut open = use_signal(|| false);

    let user_email = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();

    let user_display_name = app_state
        .user_info
        .read()
        .as_ref()
        .and_then(|u| u.display_name.clone());

    let user_photo_url = app_state
        .user_info
        .read()
        .as_ref()
        .and_then(|u| u.photo_url.clone());

    let avatar_initial = user_display_name
        .as_deref()
        .or(Some(user_email.as_str()))
        .and_then(|s| s.chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();

    rsx! {
        div {
            class: "relative",
            onmousedown: move |e| e.stop_propagation(),

            button {
                class: "w-9 h-9 flex items-center justify-center hover:bg-accent appearance-none border-none bg-transparent cursor-pointer",
                title: "Account",
                aria_label: "Open user menu",
                onclick: move |_| {
                    let next = !*open.read();
                    open.set(next);
                },
                if let Some(url) = &user_photo_url {
                    img {
                        src: "{url}",
                        alt: "User avatar",
                        class: "w-7 h-7 rounded-full object-cover",
                        referrerpolicy: "no-referrer",
                    }
                } else {
                    div {
                        class: "w-7 h-7 rounded-full flex items-center justify-center bg-primary/10 text-primary text-xs font-semibold",
                        "{avatar_initial}"
                    }
                }
            }

            if *open.read() {
                div {
                    class: "fixed inset-0 z-40",
                    onmousedown: move |e| e.stop_propagation(),
                    onclick: move |_| open.set(false),
                }
                div {
                    class: "absolute top-full right-0 mt-1 min-w-[220px] bg-background border border-border rounded-md shadow-md z-50 p-1",
                    onmousedown: move |e| e.stop_propagation(),

                    // Account summary header
                    div {
                        class: "flex items-center gap-2 px-2 py-2 border-b border-border mb-1",
                        if let Some(url) = &user_photo_url {
                            img {
                                src: "{url}",
                                alt: "User avatar",
                                class: "w-8 h-8 rounded-full object-cover shrink-0",
                                referrerpolicy: "no-referrer",
                            }
                        } else {
                            div {
                                class: "w-8 h-8 rounded-full flex items-center justify-center bg-primary/10 text-primary text-sm font-semibold shrink-0",
                                "{avatar_initial}"
                            }
                        }
                        div {
                            class: "flex flex-col min-w-0 overflow-hidden",
                            if let Some(name) = &user_display_name {
                                div { class: "text-[12px] text-foreground font-medium truncate", "{name}" }
                            }
                            div { class: "text-[11px] text-muted-foreground truncate", "{user_email}" }
                        }
                    }

                    SignOutButton {}
                }
            }
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

#[component]
fn SignOutButton() -> Element {
    let app_state = use_context::<AppState>();
    let services = app_state.services.read().clone();
    let Some(services) = services else {
        return rsx! {};
    };
    let app_state_signout = app_state.clone();
    let mut signing_out = use_signal(|| false);

    let on_sign_out = move |_| {
        if *signing_out.read() {
            return;
        }
        signing_out.set(true);
        let auth = services.auth.clone();
        let mut app_state = app_state_signout.clone();
        spawn(async move {
            auth.sign_out().await;
            app_state.is_authenticated.set(false);
            app_state.user_info.set(None);
            app_state.selected_tenant.set(None);
            app_state.selected_project.set(None);
            app_state.projects.set(Vec::new());
            app_state.upload_tasks.set(Vec::new());
        });
    };

    let is_busy = *signing_out.read();

    rsx! {
        button {
            class: "w-full h-9 px-3 text-sm rounded text-destructive bg-transparent transition ease-out hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 appearance-none border-none cursor-pointer",
            onmousedown: move |e| e.stop_propagation(),
            onclick: on_sign_out,
            disabled: is_busy,
            if is_busy {
                span { class: "spinner spinner-sm mr-1" }
                "..."
            } else {
                crate::icons::LogoutIcon {}
                "Sign Out"
            }
        }
    }
}
