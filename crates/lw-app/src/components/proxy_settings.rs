use crate::state::{AppState, CoreServices, ToastKind};
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
/// Clamp bounds for the MPU part-concurrency control. 1 lets a weak-network
/// user serialise part PUTs; 16 is a generous upper bound (past which extra
/// parallelism just thrashes and inflates peak RAM). Values outside are pinned.
const MPU_CONCURRENCY_MIN: u32 = 1;
const MPU_CONCURRENCY_MAX: u32 = 16;

#[component]
pub fn ProxySettingsPane() -> Element {
    let mut app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();
    // Treat None and Some("") identically as "blank" for the text field.
    let initial = app_state
        .config
        .read()
        .server
        .proxy_url
        .clone()
        .unwrap_or_default();
    let mut proxy_url = use_signal(|| initial);
    let initial_concurrency = app_state.config.read().upload.mpu_part_concurrency;
    let mut mpu_concurrency = use_signal(|| initial_concurrency);

    let analytics = services.analytics.clone();
    let save = move |_| {
        // Trim and normalise empty -> None so a blank field clears the
        // override rather than persisting an empty string.
        let trimmed = proxy_url.read().trim().to_string();
        // Record only whether a proxy is set, never the URL itself — the
        // value can carry host/port a user considers sensitive.
        let proxy_set = !trimmed.is_empty();
        let value = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        let mut next = app_state.config.read().clone();
        next.server.proxy_url = value;
        // Pin the concurrency into range before persisting so a hand-edited or
        // stepper-overshot value can never build a 0-permit / thrashing backend.
        next.upload.mpu_part_concurrency =
            (*mpu_concurrency.read()).clamp(MPU_CONCURRENCY_MIN, MPU_CONCURRENCY_MAX);
        match app_state.save_config(next) {
            Ok(()) => {
                analytics.capture("proxy_configured", serde_json::json!({ "set": proxy_set }));
                app_state.show_toast(
                    "Network settings saved — takes effect on next launch",
                    ToastKind::Success,
                );
            }
            Err(e) => {
                tracing::error!("Failed to save network settings: {e}");
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

            // Parallel-part concurrency for multipart uploads. Lower it (1–2) on
            // a weak/metered link so parallel PUTs don't overwhelm it.
            label {
                style: "display: block; font-size: 13px; font-weight: 500; margin-top: 16px; margin-bottom: 4px;",
                "Parallel upload parts"
            }
            div {
                style: "font-size: 12px; color: var(--text-secondary); margin-bottom: 6px;",
                "How many parts of one file upload at once (1–16). Default 6. \
                 Lower to 1–2 on a weak or metered network. Takes effect on next launch."
            }
            input {
                r#type: "number",
                min: "{MPU_CONCURRENCY_MIN}",
                max: "{MPU_CONCURRENCY_MAX}",
                step: "1",
                value: "{mpu_concurrency}",
                oninput: move |e: Event<FormData>| {
                    // Ignore an unparseable/empty transient value; keep the last
                    // good one. Range is enforced on save.
                    if let Ok(n) = e.value().parse::<u32>() {
                        mpu_concurrency.set(n);
                    }
                },
                style: "width: 100px; padding: 8px 10px; border-radius: 6px; \
                        border: 1px solid var(--border); background: var(--bg-secondary); \
                        color: var(--text); font-size: 12px; box-sizing: border-box;",
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
