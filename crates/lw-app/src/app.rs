use crate::components::login::LoginPage;
use crate::components::upload_queue::UploadQueue;
use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;

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

    if !is_authenticated {
        rsx! { LoginPage {} }
    } else {
        rsx! { MainView {} }
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
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    // Clone for closures
    let app_state_signout = app_state.clone();
    let on_sign_out = move |_| {
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

    let user_email = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();

    rsx! {
        div {
            style: "display: flex; flex-direction: column; height: 100vh; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;",

            header {
                style: "display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; border-bottom: 1px solid #e5e7eb; background: #f9fafb;",
                div {
                    style: "display: flex; align-items: center; gap: 12px;",
                    h1 { style: "margin: 0; font-size: 18px;", "Linewise Desktop" }
                    span { style: "font-size: 13px; color: #666;", "{user_email}" }
                }
                div {
                    style: "display: flex; align-items: center; gap: 12px;",
                    crate::components::tenant_select::TenantSelector {}
                    crate::components::project_select::ProjectSelector {}
                    button {
                        style: "padding: 6px 12px; border: 1px solid #ccc; border-radius: 4px; cursor: pointer; background: white; font-size: 13px;",
                        onclick: on_sign_out,
                        "Sign Out"
                    }
                }
            }

            main {
                style: "flex: 1; overflow-y: auto;",
                UploadQueue {}
            }
        }
    }
}
