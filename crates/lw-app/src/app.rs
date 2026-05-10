use crate::components::login::LoginPage;
use crate::components::upload_queue::UploadQueue;
use crate::state::{AppState, CoreServices};
use dioxus::desktop::trayicon::{init_tray_icon, menu::*};
use dioxus::prelude::*;
use lw_chat::{ChatConfig, ChatPanel};

const TAILWIND_CSS: &str = include_str!("../tailwind.generated.css");

/// Global CSS for hover/active states (can't do :hover in inline styles)
const GLOBAL_CSS: &str = r#"
/* ── CSS Variables: Light theme (default) ─────────────────────────── */
:root {
  --bg: #ffffff;
  --bg-secondary: #f9fafb;
  --bg-tertiary: #f3f4f6;
  --text: #111827;
  --text-secondary: #6b7280;
  --text-muted: #9ca3af;
  --border: #e5e7eb;
  --border-hover: #d1d5db;
  --border-focus: #2563eb;

  /* Status colors */
  --success: #22c55e;
  --success-bg: #f0fdf4;
  --error: #ef4444;
  --error-bg: #fef2f2;
  --warning: #f59e0b;
  --warning-bg: #fffbeb;
  --info: #3b82f6;
  --info-bg: #eff6ff;
  --staged-bg: #fefce8;
  --staged-border: #fde68a;
  --staged-hover: #fef9c3;

  /* Button */
  --btn-primary: #2563eb;
  --btn-primary-hover: #1d4ed8;
  --btn-primary-active: #1e40af;
  --btn-success: #22c55e;
  --btn-success-hover: #16a34a;
  --btn-success-active: #15803d;
  --btn-outline-bg: white;
  --btn-outline-hover: #f9fafb;
  --btn-outline-active: #f3f4f6;
  --btn-disabled: #e5e7eb;
  --btn-disabled-text: #9ca3af;
  --btn-danger-hover: #fef2f2;
  --btn-danger-active: #fee2e2;

  /* Input/Select */
  --input-bg: white;
  --input-border: #d1d5db;

  --scrollbar-thumb: #d1d5db;
  --scrollbar-hover: #9ca3af;
  --shadow-sm: 0 1px 3px rgba(0,0,0,0.06);
  --shadow-md: 0 1px 3px rgba(0,0,0,0.15);
  --focus-ring: 0 0 0 2px rgba(37,99,235,0.15);
}

/* ── Dark theme ───────────────────────────────────────────────────── */
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #1a1a2e;
    --bg-secondary: #16213e;
    --bg-tertiary: #1e293b;
    --text: #e2e8f0;
    --text-secondary: #94a3b8;
    --text-muted: #64748b;
    --border: #334155;
    --border-hover: #475569;
    --border-focus: #3b82f6;

    --success-bg: #052e16;
    --error-bg: #450a0a;
    --warning-bg: #451a03;
    --info-bg: #172554;
    --staged-bg: #422006;
    --staged-border: #854d0e;
    --staged-hover: #4a2506;

    --btn-outline-bg: #1e293b;
    --btn-outline-hover: #334155;
    --btn-outline-active: #475569;
    --btn-disabled: #334155;
    --btn-disabled-text: #64748b;
    --btn-danger-hover: #450a0a;
    --btn-danger-active: #7f1d1d;

    --input-bg: #1e293b;
    --input-border: #475569;

    --scrollbar-thumb: #475569;
    --scrollbar-hover: #64748b;
    --shadow-sm: 0 1px 3px rgba(0,0,0,0.3);
    --shadow-md: 0 1px 3px rgba(0,0,0,0.4);
    --focus-ring: 0 0 0 2px rgba(59,130,246,0.25);
  }
}

/* ── Base ─────────────────────────────────────────────────────────── */
@layer base {
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body { height: 100%; overflow: hidden; }
}
body {
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
  font-size: 14px;
  color: var(--text);
  background: var(--bg);
}

/* ── Button hover/active ─────────────────────────────────────────── */
.btn-primary:hover { background: var(--btn-primary-hover) !important; box-shadow: var(--shadow-md); }
.btn-primary:active { background: var(--btn-primary-active) !important; transform: scale(0.97); }
.btn-success:hover { background: var(--btn-success-hover) !important; box-shadow: var(--shadow-md); }
.btn-success:active { background: var(--btn-success-active) !important; transform: scale(0.97); }
.btn-outline:hover { background: var(--btn-outline-hover) !important; border-color: var(--border-hover) !important; }
.btn-outline:active { background: var(--btn-outline-active) !important; transform: scale(0.97); }
/* Login separator */
.login-separator { display: flex; align-items: center; gap: 12px; width: 100%; }
.login-separator::before, .login-separator::after { content: ''; flex: 1; height: 1px; background: var(--color-border); }
.btn-danger-sm:hover { background: var(--btn-danger-hover) !important; border-color: var(--error) !important; }
.btn-danger-sm:active { background: var(--btn-danger-active) !important; transform: scale(0.97); }

/* ── Form controls ───────────────────────────────────────────────── */
select { background: var(--input-bg); color: var(--text); border-color: var(--input-border); }
select:hover { border-color: var(--border-hover); }
select:focus { border-color: var(--border-focus); box-shadow: var(--focus-ring); outline: none; }
input { background: var(--input-bg) !important; color: var(--text) !important; border-color: var(--input-border) !important; }
input:focus { border-color: var(--border-focus) !important; box-shadow: var(--focus-ring) !important; outline: none; }

/* ── Card / row hover ────────────────────────────────────────────── */
.card-row:hover { border-color: var(--border-hover) !important; box-shadow: var(--shadow-sm); }
.staged-row:hover { background: var(--staged-hover) !important; }

/* ── Scrollbar ───────────────────────────────────────────────────── */
::-webkit-scrollbar { width: 6px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: var(--scrollbar-thumb); border-radius: 3px; }
::-webkit-scrollbar-thumb:hover { background: var(--scrollbar-hover); }

/* ── Animations ──────────────────────────────────────────────────── */
.fade-in { animation: fadeIn 0.2s ease-out; }
@keyframes fadeIn { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: translateY(0); } }

.slide-in-right { animation: slideInRight 0.25s ease-out; }
@keyframes slideInRight { from { opacity: 0; transform: translateX(20px); } to { opacity: 1; transform: translateX(0); } }

.slide-down { animation: slideDown 0.2s ease-out; }
@keyframes slideDown { from { opacity: 0; transform: translateY(-8px); } to { opacity: 1; transform: translateY(0); } }

.fade-in-left { animation: fadeInLeft 0.2s ease-out; }
@keyframes fadeInLeft { from { opacity: 0; transform: translateX(-12px); } to { opacity: 1; transform: translateX(0); } }

/* Spinner for loading states */
.spinner {
    display: inline-block; width: 14px; height: 14px;
    border: 2px solid transparent; border-top-color: currentColor; border-radius: 50%;
    animation: spin 0.6s linear infinite; vertical-align: middle;
}
.spinner-sm { width: 12px; height: 12px; border-width: 1.5px; }
@keyframes spin { to { transform: rotate(360deg); } }

/* Loading overlay for full-page states */
.loading-screen {
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    height: 100vh; gap: 12px; color: var(--text-secondary);
}
.loading-screen .spinner { width: 24px; height: 24px; border-width: 2.5px; color: var(--btn-primary); }

/* Smooth transitions for collapsible content */
.collapse-arrow { display: inline-block; transition: transform 0.15s ease; }
.collapse-arrow.open { transform: rotate(90deg); }

/* Stagger delay utility — set via inline style: animation-delay: Xms */
.stagger { animation: fadeIn 0.2s ease-out backwards; }
"#;

#[component]
pub fn App() -> Element {
    use_context_provider(AppState::new);
    use_context_provider(|| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(CoreServices::init())
                .expect("failed to initialize core services")
        })
    });

    // Initialize system tray
    use_hook(|| {
        let menu = build_tray_menu();
        init_tray_icon(menu, None);
    });

    // Handle tray menu events
    dioxus::desktop::use_tray_menu_event_handler(move |event| match event.id().0.as_str() {
        "show" => {
            let window = dioxus::desktop::window();
            window.set_visible(true);
            window.set_focus();
        }
        "quit" => std::process::exit(0),
        _ => {}
    });

    // Handle tray icon click — show window
    dioxus::desktop::use_tray_icon_event_handler(move |_event| {
        let window = dioxus::desktop::window();
        window.set_visible(true);
        window.set_focus();
    });

    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();
    let mut restoring = use_signal(|| true);

    // Try to restore session on first render
    let app_state_restore = app_state.clone();
    use_future(move || {
        let auth = services.auth.clone();
        let api = services.api.clone();
        let mut app_state = app_state_restore.clone();
        async move {
            if let Ok(_tokens) = auth.try_restore_session().await {
                tracing::info!("Session restored");
                if let Ok(token) = auth.get_id_token().await {
                    app_state.auth_token.set(token);
                }
                fetch_user_info(&api, &mut app_state).await;
            }
            restoring.set(false);
        }
    });

    let is_authenticated = *app_state.is_authenticated.read();
    let is_restoring = *restoring.read();

    rsx! {
        style { "{GLOBAL_CSS}" }
        style { "{TAILWIND_CSS}" }
        style { "{lw_chat::styles::CHAT_CSS}" }
        if is_restoring {
            div { class: "loading-screen",
                span { class: "spinner" }
                span { "Signing in..." }
            }
        } else if !is_authenticated {
            LoginPage {}
        } else {
            MainView {}
        }
    }
}

async fn fetch_user_info(api: &lw_core::api_client::ApiClient, app_state: &mut AppState) {
    match api.whoami().await {
        Ok(resp) => {
            if let Some(info) = lw_core::models::UserInfo::from_whoami(resp) {
                // Set Sentry user context for error attribution
                sentry::configure_scope(|scope| {
                    scope.set_user(Some(sentry::User {
                        id: Some(info.uid.clone()),
                        email: Some(info.email.clone()),
                        username: info.display_name.clone(),
                        ..Default::default()
                    }));
                });
                app_state.user_info.set(Some(info));
                app_state.is_authenticated.set(true);
            } else {
                tracing::warn!("WhoAmI response has no user");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to fetch user info: {e}");
        }
    }
}

#[component]
fn MainView() -> Element {
    let mut chat_open = use_signal(|| false);
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    let mut tenant_id = use_signal(String::new);
    let mut project_id_sig: Signal<Option<String>> = use_signal(|| None);

    // Sync tenant/project selections to chat config signals
    use_effect(move || {
        let t = app_state
            .selected_tenant
            .read()
            .as_ref()
            .map(|t| t.id.clone())
            .unwrap_or_default();
        tenant_id.set(t);
    });

    use_effect(move || {
        let p = app_state
            .selected_project
            .read()
            .as_ref()
            .map(|p| p.id.clone());
        project_id_sig.set(p);
    });

    let chat_config = ChatConfig {
        base_url: services
            .config
            .server
            .environment
            .api_base_url()
            .to_string(),
        auth_token: app_state.auth_token,
        tenant: tenant_id,
        project_id: project_id_sig,
    };

    let is_open = *chat_open.read();


    rsx! {
        div {
            style: "display: flex; height: 100vh; position: relative;",

            crate::components::sidebar::Sidebar {}

            // Main content — upload queue
            div {
                style: "flex: 1; display: flex; flex-direction: column; overflow: hidden; min-width: 0; position: relative;",

                main {
                    style: "flex: 1; overflow-y: auto; padding: 16px;",
                    UploadQueue {}
                }

                button {
                    style: "position: absolute; bottom: 24px; right: 24px; z-index: 100; \
                            width: 48px; height: 48px; border-radius: 50%; \
                            display: flex; align-items: center; justify-content: center; \
                            background: var(--btn-primary, #5C01DA); color: white; \
                            border: none; cursor: pointer; \
                            box-shadow: 0 4px 12px rgba(0,0,0,0.2); \
                            transition: background 0.15s, transform 0.15s;",
                    onclick: move |_| chat_open.set(!is_open),
                    title: if is_open { "Close Chat" } else { "Ask Linus" },
                    if is_open {
                        crate::icons::CloseIcon {}
                    } else {
                        crate::icons::ChatIcon {}
                    }
                }
            }

            // Right panel — chat
            if is_open {
                div {
                    class: "slide-in-right",
                    style: "width: 380px; flex-shrink: 0; border-left: 1px solid var(--border); \
                            display: flex; flex-direction: column; overflow: hidden;",
                    ChatPanel { config: chat_config }
                }
            }
        }
    }
}

fn build_tray_menu() -> dioxus::desktop::trayicon::DioxusTrayMenu {
    let menu = Menu::new();
    let show = MenuItem::with_id("show", "Open Linewise", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit])
        .expect("failed to build tray menu");
    menu
}
