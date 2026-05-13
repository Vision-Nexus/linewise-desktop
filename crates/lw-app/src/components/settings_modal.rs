use crate::components::general_settings::GeneralSettingsPane;
use crate::components::transcode_settings::TranscodeSettingsPane;
use crate::components::upload_settings::UploadSettingsPane;
use crate::state::AppState;
use dioxus::prelude::*;

/// Attribution file baked into the binary so the About pane can render
/// it regardless of installer layout. The same file is also shipped
/// under `licenses/` inside each installer (see `scripts/bundle-*`).
const THIRD_PARTY_LICENSES: &str = include_str!("../../../../THIRD_PARTY_LICENSES.md");

const PROJECT_URL: &str = "https://github.com/Vision-Nexus/linewise-desktop";

#[component]
pub fn SettingsModal(on_close: EventHandler<()>) -> Element {
    let close = on_close;
    rsx! {
        div {
            style: "position: fixed; inset: 0; background: rgba(0,0,0,0.4); z-index: 100; \
                    display: flex; align-items: center; justify-content: center;",
            onclick: move |_| close.call(()),

            div {
                style: "background: var(--bg); border: 1px solid var(--border); border-radius: 8px; \
                        width: 560px; max-width: 90vw; max-height: 85vh; overflow-y: auto; \
                        padding: 20px; color: var(--text); box-shadow: var(--shadow-md);",
                onclick: move |e| e.stop_propagation(),

                // Header
                div {
                    style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px;",
                    h2 { style: "margin: 0; font-size: 18px; font-weight: 600;", "Settings" }
                    button {
                        style: "background: none; border: none; color: var(--text-muted); \
                                cursor: pointer; font-size: 20px; padding: 4px; line-height: 1;",
                        onclick: move |_| close.call(()),
                        "×"
                    }
                }

                // Account section
                SectionHeader { label: "Account" }
                AccountPane {}

                // Divider
                div { style: "height: 1px; background: var(--border); margin: 20px 0;" }

                // Upload section
                SectionHeader { label: "Upload" }
                UploadSettingsPane {}

                // Divider
                div { style: "height: 1px; background: var(--border); margin: 20px 0;" }

                // Transcode section
                SectionHeader { label: "Transcode" }
                TranscodeSettingsPane {}

                // Divider
                div { style: "height: 1px; background: var(--border); margin: 20px 0;" }

                // General section (logging, etc.)
                SectionHeader { label: "General" }
                GeneralSettingsPane {}

                // Divider
                div { style: "height: 1px; background: var(--border); margin: 20px 0;" }

                // About & Notices
                SectionHeader { label: "About & Notices" }
                AboutPane {}
            }
        }
    }
}

#[component]
fn AboutPane() -> Element {
    let version = env!("CARGO_PKG_VERSION");
    rsx! {
        div {
            style: "font-size: 13px; color: var(--text); margin-bottom: 8px;",
            div { style: "font-weight: 600;", "Linewise Desktop {version}" }
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
    let tenants: String = user
        .tenants
        .iter()
        .map(|t| t.display_name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let tenants_display = if tenants.is_empty() {
        "—".to_string()
    } else {
        tenants
    };

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
                            background: rgba(37,99,235,0.1); color: var(--btn-primary); \
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
        InfoRow { label: "Organizations", value: tenants_display }
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
