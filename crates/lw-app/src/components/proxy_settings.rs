use crate::state::{AppState, ToastKind};
use dioxus::prelude::*;

/// Optional fixed HTTP proxy for every outbound client (API, auth, GCS
/// uploads). Persisted to `config.toml` via `ServerConfig::proxy_url`.
///
/// The three reqwest clients are built once at startup and live for the
/// whole session, so a change here takes effect on next launch — the hint
/// says so and we do **not** attempt a hot client rebuild. An empty field
/// clears the override and restores the system-default behaviour.
///
/// Users running v2ray flip GLOBAL↔RULE mid-upload; the Windows
/// system-proxy snapshot the clients captured at launch then goes stale and
/// uploads wedge until restart. Pointing this at v2ray's stable local HTTP
/// inbound (e.g. `http://127.0.0.1:10809`) survives those mode switches, so
/// the uploaders' retry loops recover.
#[component]
pub fn ProxySettingsPane() -> Element {
    let mut app_state = use_context::<AppState>();
    // Treat None and Some("") identically as "blank" for the text field.
    let initial = app_state
        .config
        .read()
        .server
        .proxy_url
        .clone()
        .unwrap_or_default();
    let mut proxy_url = use_signal(|| initial);

    let save = move |_| {
        // Trim and normalise empty -> None so a blank field clears the
        // override rather than persisting an empty string.
        let trimmed = proxy_url.read().trim().to_string();
        let value = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        let mut next = app_state.config.read().clone();
        next.server.proxy_url = value;
        match app_state.save_config(next) {
            Ok(()) => {
                app_state.show_toast(
                    "Proxy saved — takes effect on next launch",
                    ToastKind::Success,
                );
            }
            Err(e) => {
                tracing::error!("Failed to save proxy_url: {e}");
                app_state.show_toast(format!("Failed to save settings: {e}"), ToastKind::Error);
            }
        }
    };

    let clear = move |_| {
        proxy_url.set(String::new());
    };

    rsx! {
        div {
            style: "background: var(--bg); color: var(--text);",

            label {
                style: "display: block; font-size: 13px; font-weight: 500; margin-bottom: 4px;",
                "HTTP Proxy (optional)"
            }
            div {
                style: "font-size: 12px; color: var(--text-secondary); margin-bottom: 6px;",
                "e.g. ",
                code { style: "font-family: ui-monospace, SFMono-Regular, Menlo, monospace;",
                    "http://127.0.0.1:10809"
                },
                " — point at your v2ray HTTP inbound; leave blank to use the system default. \
                 HTTP/HTTPS only (no SOCKS). Takes effect on next launch."
            }
            input {
                r#type: "text",
                value: "{proxy_url}",
                placeholder: "http://127.0.0.1:10809",
                spellcheck: "false",
                autocapitalize: "off",
                autocorrect: "off",
                oninput: move |e: Event<FormData>| proxy_url.set(e.value()),
                style: "width: 100%; padding: 8px 10px; border-radius: 6px; \
                        border: 1px solid var(--border); background: var(--bg-secondary); \
                        color: var(--text); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; \
                        font-size: 12px; box-sizing: border-box;",
            }

            div {
                style: "display: flex; gap: 8px; margin-top: 12px;",
                button {
                    style: "flex: 1; padding: 8px 16px; border-radius: 6px; border: none; \
                            background: var(--btn-primary); color: white; cursor: pointer; \
                            font-weight: 500; font-size: 13px;",
                    onclick: save,
                    "Save"
                }
                button {
                    style: "padding: 8px 16px; border-radius: 6px; border: 1px solid var(--border); \
                            background: transparent; color: var(--text-secondary); cursor: pointer; \
                            font-size: 13px;",
                    onclick: clear,
                    "Clear"
                }
            }
        }
    }
}
