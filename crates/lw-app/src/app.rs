use crate::components::login::LoginPage;
use crate::components::upload_queue::UploadQueue;
use crate::state::{AppState, CoreServices};
use dioxus::desktop::trayicon::{init_tray_icon, menu::*};
use dioxus::prelude::*;

/// Global CSS for hover/active states (can't do :hover in inline styles)
const GLOBAL_CSS: &str = r#"
* { box-sizing: border-box; margin: 0; padding: 0; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif; font-size: 14px; color: #111827; background: #ffffff; }

/* Button hover/active animations */
.btn-primary:hover { background: #1d4ed8 !important; box-shadow: 0 1px 3px rgba(0,0,0,0.15); }
.btn-primary:active { background: #1e40af !important; transform: scale(0.97); }
.btn-success:hover { background: #16a34a !important; box-shadow: 0 1px 3px rgba(0,0,0,0.15); }
.btn-success:active { background: #15803d !important; transform: scale(0.97); }
.btn-outline:hover { background: #f9fafb !important; border-color: #9ca3af !important; }
.btn-outline:active { background: #f3f4f6 !important; transform: scale(0.97); }
.btn-danger-sm:hover { background: #fef2f2 !important; border-color: #ef4444 !important; }
.btn-danger-sm:active { background: #fee2e2 !important; transform: scale(0.97); }

/* Select hover */
select:hover { border-color: #9ca3af; }
select:focus { border-color: #2563eb; box-shadow: 0 0 0 2px rgba(37,99,235,0.15); outline: none; }

/* Input focus */
input:focus { border-color: #2563eb !important; box-shadow: 0 0 0 2px rgba(37,99,235,0.15) !important; outline: none; }

/* Card row hover */
.card-row:hover { border-color: #d1d5db !important; box-shadow: 0 1px 3px rgba(0,0,0,0.06); }

/* Staged row hover */
.staged-row:hover { background: #fef9c3 !important; }

/* Scrollbar styling */
::-webkit-scrollbar { width: 6px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: #d1d5db; border-radius: 3px; }
::-webkit-scrollbar-thumb:hover { background: #9ca3af; }

/* Smooth transitions for state changes */
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
