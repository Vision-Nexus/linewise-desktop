use crate::components::login::LoginPage;
use crate::components::upload_queue::UploadQueue;
use crate::state::{AppState, CoreServices};
use dioxus::desktop::trayicon::{init_tray_icon, menu::*};
use dioxus::prelude::*;

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
* { box-sizing: border-box; margin: 0; padding: 0; }
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
.fade-in { animation: fadeIn 0.2s ease-in; }
@keyframes fadeIn { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: translateY(0); } }
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
    dioxus::desktop::use_tray_menu_event_handler(move |event| {
        match event.id().0.as_str() {
            "show" => {
                let window = dioxus::desktop::window();
                window.set_visible(true);
                window.set_focus();
            }
            "quit" => std::process::exit(0),
            _ => {}
        }
    });

    // Handle tray icon click — show window
    dioxus::desktop::use_tray_icon_event_handler(move |_event| {
        let window = dioxus::desktop::window();
        window.set_visible(true);
        window.set_focus();
    });

    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    // Try to restore session on first render
    let app_state_restore = app_state.clone();
    use_future(move || {
        let auth = services.auth.clone();
        let api = services.api.clone();
        let mut app_state = app_state_restore.clone();
        async move {
            if let Ok(_tokens) = auth.try_restore_session().await {
                tracing::info!("Session restored");
                fetch_user_info(&api, &mut app_state).await;
            }
        }
    });

    let is_authenticated = *app_state.is_authenticated.read();

    rsx! {
        style { "{GLOBAL_CSS}" }
        if !is_authenticated {
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
    rsx! {
        div {
            style: "display: flex; height: 100vh;",

            // Fixed-width sidebar with tenant/project selectors
            crate::components::sidebar::Sidebar {}

            // Flexible main content
            main {
                style: "flex: 1; overflow-y: auto; padding: 16px;",
                UploadQueue {}
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
