use crate::state::{AppState, ToastKind};
use dioxus::prelude::*;
use lw_core::config::Environment;

/// Admin-only environment switcher. Visible when the signed-in user
/// holds at least one `systemRoles` entry in their Firebase token.
/// Persists the chosen environment to `config.toml` and asks the
/// bootstrap effect to rebuild `CoreServices`, which swaps the
/// `ApiClient` base URL. The Firebase auth state, the local SQLite
/// database, and the keyring-stored refresh token all stay — only
/// the API surface changes — so the user does not have to sign in
/// again. Cached tenant lists are refetched on the next whoami.
#[component]
pub fn EnvironmentSettingsPane() -> Element {
    let mut app_state = use_context::<AppState>();
    let initial = app_state.config.read().server.environment;
    let selected = use_signal(|| initial);

    let switch = move |_| {
        let target = *selected.read();
        let current = app_state.config.read().server.environment;
        if current == target {
            app_state.show_toast(format!("Already on {}", target.label()), ToastKind::Info);
            return;
        }
        let mut next = app_state.config.read().clone();
        next.server.environment = target;
        match app_state.save_config(next) {
            Ok(()) => {
                // Drop tenant/project caches that belong to the outgoing
                // environment — those IDs may not exist in the new one
                // and the sidebar would otherwise render dangling
                // selections.
                app_state.selected_tenant.set(None);
                app_state.selected_project.set(None);
                app_state.projects.set(Vec::new());
                app_state.tenant_projects.set(Default::default());
                app_state.show_toast(
                    format!("Switching to {}…", target.label()),
                    ToastKind::Info,
                );
                app_state.request_restart();
            }
            Err(e) => {
                tracing::error!("Failed to save environment: {e}");
                app_state.show_toast(format!("Failed to save: {e}"), ToastKind::Error);
            }
        }
    };

    rsx! {
        div {
            style: "background: var(--bg); color: var(--text);",

            div {
                style: "font-size: 12px; color: var(--text-secondary); margin-bottom: 10px;",
                "Switch which Linewise backend the desktop talks to. \
                 Visible because your account has system-level roles."
            }

            EnvRow {
                env: Environment::Production,
                label: "Production",
                host: "api.product.linewise.io",
                selected: selected,
            }
            EnvRow {
                env: Environment::Testing,
                label: "Testing",
                host: "api.testing.linewise.io",
                selected: selected,
            }
            EnvRow {
                env: Environment::Dev,
                label: "Dev",
                host: "api.dev.linewise.io",
                selected: selected,
            }

            div {
                style: "margin-top: 12px;",
                button {
                    style: "padding: 8px 16px; border-radius: 6px; border: none; \
                            background: var(--btn-primary); color: white; cursor: pointer; \
                            font-weight: 500; font-size: 13px;",
                    onclick: switch,
                    "Switch and reload"
                }
            }
        }
    }
}

#[component]
fn EnvRow(
    env: Environment,
    label: &'static str,
    host: &'static str,
    selected: Signal<Environment>,
) -> Element {
    let is_selected = *selected.read() == env;
    let mut sel = selected;
    rsx! {
        label {
            style: "display: flex; gap: 10px; align-items: center; padding: 8px 10px; \
                    border: 1px solid var(--border); border-radius: 6px; margin-bottom: 6px; \
                    cursor: pointer; background: var(--bg-secondary);",
            input {
                r#type: "radio",
                name: "environment",
                checked: is_selected,
                onchange: move |_| sel.set(env),
                style: "margin: 0;",
            }
            div {
                style: "flex: 1;",
                div { style: "font-size: 13px; font-weight: 500;", "{label}" }
                div {
                    style: "font-family: ui-monospace, SFMono-Regular, Menlo, monospace; \
                            font-size: 11px; color: var(--text-secondary);",
                    "{host}"
                }
            }
        }
    }
}
