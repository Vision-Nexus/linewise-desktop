use crate::components::environment_settings::EnvironmentSettingsPane;
use crate::components::general_settings::GeneralSettingsPane;
use crate::components::proxy_settings::ProxySettingsPane;
use crate::components::transcode_settings::TranscodeSettingsPane;
use crate::components::upload_settings::UploadSettingsPane;
use crate::components::version_banner::open_release_page;
use crate::state::AppState;
use dioxus::prelude::*;
use lw_core::version_check::VersionStatus;

/// Tab styles live in a class instead of inline `style` because the
/// inline approach was unreliable across re-renders: switching tabs
/// updated font-weight and color but left background and border-left
/// stuck on the previous tab's value, regardless of `!important`.
/// Class toggling sidesteps Dioxus' per-property style diff entirely.
const SETTINGS_TAB_CSS: &str = r#"
.lw-settings-tab {
    background: transparent;
    border: none;
    border-left: 2px solid transparent;
    cursor: pointer;
    padding: 8px 16px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text-secondary);
    text-align: left;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
}
.lw-settings-tab.is-active {
    background: var(--bg);
    color: var(--text);
    font-weight: 600;
    border-left-color: var(--btn-primary);
}
"#;

/// Attribution file baked into the binary so the About pane can render
/// it regardless of installer layout. The same file is also shipped
/// under `licenses/` inside each installer (see `crates/xtask/`).
const THIRD_PARTY_LICENSES: &str = include_str!("../../../../THIRD_PARTY_LICENSES.md");

const PROJECT_URL: &str = "https://github.com/Vision-Nexus/linewise-desktop";

/// Top-level grouping for the settings modal. Three tabs:
///
/// * **General** — the things ordinary users touch: account info,
///   upload behaviour, transcode preferences.
/// * **Advanced** — operational/diagnostic toggles. Environment
///   switcher (system admins only) and the tracing log filter; a user
///   coming here usually has a specific reason.
/// * **About** — version, license, third-party notices.
///
/// The active tab is local component state — losing it on close is
/// fine, the next open should re-anchor on General.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Advanced,
    About,
}

#[component]
pub fn SettingsModal(on_close: EventHandler<()>) -> Element {
    let close = on_close;
    let mut tab = use_signal(|| Tab::General);
    rsx! {
        style { "{SETTINGS_TAB_CSS}" }
        div {
            style: "position: fixed; inset: 0; background: rgba(0,0,0,0.4); z-index: 100; \
                    display: flex; align-items: center; justify-content: center;",
            onclick: move |_| close.call(()),

            div {
                style: "background: var(--bg); border: 1px solid var(--border); border-radius: 8px; \
                        width: 760px; max-width: 92vw; height: 85vh; \
                        display: flex; flex-direction: row; \
                        color: var(--text); box-shadow: var(--shadow-md); overflow: hidden;",
                onclick: move |e| e.stop_propagation(),

                // Left rail — vertical tab list. Fixed width so the
                // active-state border doesn't shift when label widths
                // change between tabs.
                nav {
                    style: "flex: 0 0 180px; display: flex; flex-direction: column; \
                            padding: 16px 0; gap: 2px; \
                            border-right: 1px solid var(--border); background: var(--bg-secondary);",
                    div {
                        style: "padding: 0 16px 12px 16px; \
                                font-size: 11px; font-weight: 600; text-transform: uppercase; \
                                letter-spacing: 0.06em; color: var(--text-muted);",
                        "Settings"
                    }
                    TabButton {
                        label: "General",
                        active: *tab.read() == Tab::General,
                        onclick: move |_| tab.set(Tab::General),
                    }
                    TabButton {
                        label: "Advanced",
                        active: *tab.read() == Tab::Advanced,
                        onclick: move |_| tab.set(Tab::Advanced),
                    }
                    TabButton {
                        label: "About",
                        active: *tab.read() == Tab::About,
                        onclick: move |_| tab.set(Tab::About),
                    }
                }

                // Right column — title + scrollable body.
                div {
                    style: "flex: 1; min-width: 0; display: flex; flex-direction: column;",

                    // Header — title of the active tab plus close button.
                    div {
                        style: "display: flex; justify-content: space-between; align-items: center; \
                                padding: 16px 20px; border-bottom: 1px solid var(--border); flex-shrink: 0;",
                        h2 {
                            style: "margin: 0; font-size: 18px; font-weight: 600;",
                            "{tab_title(*tab.read())}"
                        }
                        button {
                            style: "background: none; border: none; color: var(--text-muted); \
                                    cursor: pointer; font-size: 20px; padding: 4px; line-height: 1;",
                            onclick: move |_| close.call(()),
                            "×"
                        }
                    }

                    // Tab body — only this scrolls so the title and the
                    // left rail stay anchored when the active pane is
                    // long.
                    div {
                        style: "flex: 1; min-height: 0; overflow-y: auto; padding: 20px;",
                        match *tab.read() {
                            Tab::General => rsx! { GeneralTab {} },
                            Tab::Advanced => rsx! { AdvancedTab {} },
                            Tab::About => rsx! { AboutTab {} },
                        }
                    }
                }
            }
        }
    }
}

fn tab_title(tab: Tab) -> &'static str {
    match tab {
        Tab::General => "General",
        Tab::Advanced => "Advanced",
        Tab::About => "About",
    }
}

#[component]
fn TabButton(label: &'static str, active: bool, onclick: EventHandler<MouseEvent>) -> Element {
    let class = if active {
        "lw-settings-tab is-active"
    } else {
        "lw-settings-tab"
    };
    rsx! {
        button {
            class: "{class}",
            onclick: move |e| onclick.call(e),
            "{label}"
        }
    }
}

#[component]
fn GeneralTab() -> Element {
    rsx! {
        SectionHeader { label: "Account" }
        AccountPane {}

        Divider {}

        SectionHeader { label: "Upload" }
        UploadSettingsPane {}

        Divider {}

        SectionHeader { label: "Transcode" }
        TranscodeSettingsPane {}
    }
}

#[component]
fn AdvancedTab() -> Element {
    rsx! {
        // Environment switcher is gated on `systemRoles` non-empty.
        // Ordinary users still see the Advanced tab and the log filter
        // below — the tab itself isn't admin-only, only this pane is.
        if is_system_user() {
            SectionHeader { label: "Environment (system admin)" }
            EnvironmentSettingsPane {}

            Divider {}
        }

        SectionHeader { label: "Network" }
        ProxySettingsPane {}

        Divider {}

        SectionHeader { label: "Logging" }
        GeneralSettingsPane {}
    }
}

#[component]
fn AboutTab() -> Element {
    rsx! {
        SectionHeader { label: "About & Notices" }
        AboutPane {}
    }
}

#[component]
fn Divider() -> Element {
    rsx! {
        div { style: "height: 1px; background: var(--border); margin: 20px 0;" }
    }
}

#[component]
fn AboutPane() -> Element {
    let version = env!("CARGO_PKG_VERSION");
    let app_state = use_context::<AppState>();
    let status = app_state.version_status.read().clone();
    // The optional release_url accompanies UpdateAvailable and Unsupported. We
    // mirror the banner's CTA here so a user who dismissed the banner (or who
    // sits on the login page where the banner is the only update affordance)
    // still has a one-click path to the release page.
    let (status_label, status_color, release_url) = match &status {
        None => (
            "Checking for updates…".to_string(),
            "var(--text-secondary)",
            None,
        ),
        Some(VersionStatus::UpToDate { .. }) => ("Up to date".to_string(), "var(--success)", None),
        Some(VersionStatus::UpdateAvailable {
            latest,
            release_url,
            ..
        }) => (
            format!("Update available — v{latest}"),
            "var(--info)",
            Some(release_url.clone()),
        ),
        Some(VersionStatus::Unsupported {
            running,
            min_supported,
            release_url,
            ..
        }) => (
            format!("Unsupported — running v{running}, requires v{min_supported}"),
            "var(--error)",
            Some(release_url.clone()),
        ),
    };

    rsx! {
        div {
            style: "font-size: 13px; color: var(--text); margin-bottom: 8px;",
            div { style: "font-weight: 600;", "Linewise Desktop {version}" }
            div { style: "font-size: 12px; color: {status_color}; margin-top: 4px; \
                          display: flex; align-items: center; gap: 8px;",
                span { "{status_label}" }
                if let Some(url) = release_url {
                    button {
                        style: "appearance: none; background: transparent; border: none; \
                                padding: 0; cursor: pointer; color: var(--btn-primary); \
                                font-size: 12px; text-decoration: underline;",
                        onclick: move |_| open_release_page(url.clone()),
                        "Open release page"
                    }
                }
            }
            div { style: "font-size: 12px; color: var(--text-secondary); margin-top: 4px;",
                "Released under the GNU GPLv2-or-later. Source at "
                a {
                    href: "{PROJECT_URL}",
                    style: "color: var(--btn-primary);",
                    "{PROJECT_URL}"
                }
                "."
            }
        }

        div {
            style: "margin-top: 12px; padding: 12px; border: 1px solid var(--border); \
                    border-radius: 6px; background: var(--bg-secondary); \
                    max-height: 280px; overflow-y: auto;",
            pre {
                style: "font-family: ui-monospace, SFMono-Regular, Menlo, monospace; \
                        font-size: 11px; line-height: 1.5; color: var(--text); \
                        white-space: pre-wrap; word-break: break-word; margin: 0;",
                "{THIRD_PARTY_LICENSES}"
            }
        }
    }
}

#[component]
fn SectionHeader(label: &'static str) -> Element {
    rsx! {
        h3 {
            style: "margin: 0 0 12px 0; font-size: 13px; font-weight: 600; \
                    text-transform: uppercase; letter-spacing: 0.04em; color: var(--text-secondary);",
            "{label}"
        }
    }
}

/// Read the signed-in user's system-role status from `AppState`.
/// Defaults to `false` when no user is loaded yet (e.g. settings
/// opened from a recovery screen) — better to hide the admin
/// affordance than to flash it for a moment.
fn is_system_user() -> bool {
    use_context::<AppState>()
        .user_info
        .read()
        .as_ref()
        .map(|u| u.is_system_user())
        .unwrap_or(false)
}

#[component]
fn AccountPane() -> Element {
    let app_state = use_context::<AppState>();
    let user_info = app_state.user_info.read();
    let Some(user) = user_info.as_ref() else {
        return rsx! {
            div {
                style: "font-size: 13px; color: var(--text-muted);",
                "Not signed in."
            }
        };
    };

    let display_name = user.display_name.clone().unwrap_or_else(|| "—".to_string());
    let avatar_initial = user
        .display_name
        .as_deref()
        .or(Some(user.email.as_str()))
        .and_then(|s| s.chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let tenants: Vec<String> = user
        .tenants
        .iter()
        .map(|t| t.display_name.clone())
        .collect();

    rsx! {
        div {
            style: "display: flex; gap: 16px; align-items: center; margin-bottom: 8px;",
            if let Some(url) = &user.photo_url {
                img {
                    src: "{url}",
                    alt: "User avatar",
                    style: "width: 48px; height: 48px; border-radius: 9999px; object-fit: cover; flex-shrink: 0;",
                    referrerpolicy: "no-referrer",
                }
            } else {
                div {
                    style: "width: 48px; height: 48px; border-radius: 9999px; flex-shrink: 0; \
                            display: flex; align-items: center; justify-content: center; \
                            background: var(--bg-secondary); color: var(--text); \
                            font-size: 18px; font-weight: 600;",
                    "{avatar_initial}"
                }
            }
            div {
                style: "flex: 1; min-width: 0;",
                div { style: "font-size: 14px; font-weight: 600;", "{display_name}" }
                div { style: "font-size: 12px; color: var(--text-secondary);", "{user.email}" }
            }
        }

        InfoRow { label: "User ID", value: user.uid.clone() }
        OrganizationsRow { tenants }
    }
}

/// Vertical, scrollable list of the user's tenant memberships. Users
/// who belong to many orgs would otherwise overflow the row
/// horizontally (the comma-joined string the previous version
/// rendered would either truncate or push the column too wide). The
/// max-height + overflow-y caps the visual footprint regardless of
/// how many orgs there are.
#[component]
fn OrganizationsRow(tenants: Vec<String>) -> Element {
    let count = tenants.len();
    rsx! {
        div {
            style: "display: flex; gap: 12px; padding: 6px 0; font-size: 13px;",
            div {
                style: "flex: 0 0 120px; color: var(--text-secondary);",
                "Organizations"
            }
            div {
                style: "flex: 1; min-width: 0;",
                if count == 0 {
                    div {
                        style: "color: var(--text-muted); font-size: 12px;",
                        "—"
                    }
                } else {
                    div {
                        style: "max-height: 140px; overflow-y: auto; \
                                border: 1px solid var(--border); border-radius: 4px; \
                                padding: 4px 0; background: var(--bg-secondary);",
                        for name in tenants.iter() {
                            div {
                                style: "padding: 4px 10px; font-size: 12px; \
                                        color: var(--text); word-break: break-word;",
                                "{name}"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn InfoRow(label: &'static str, value: String) -> Element {
    rsx! {
        div {
            style: "display: flex; gap: 12px; padding: 6px 0; font-size: 13px;",
            div {
                style: "flex: 0 0 120px; color: var(--text-secondary);",
                "{label}"
            }
            div {
                style: "flex: 1; color: var(--text); word-break: break-all; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px;",
                "{value}"
            }
        }
    }
}
