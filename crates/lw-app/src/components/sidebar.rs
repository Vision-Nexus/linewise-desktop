use crate::state::AppState;
use crate::styles;
use dioxus::prelude::*;

#[component]
pub fn Sidebar() -> Element {
    let app_state = use_context::<AppState>();
    let user_email = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();

    rsx! {
        aside {
            style: "width: {styles::SIDEBAR_WIDTH}px; height: 100vh; display: flex; flex-direction: column; border-right: 1px solid var(--border); background: var(--bg-secondary); flex-shrink: 0;",

            // App title
            div {
                style: "height: {styles::TOPBAR_HEIGHT}px; display: flex; align-items: center; padding: 0 16px; border-bottom: 1px solid var(--border);",
                h1 { style: "font-size: 15px; font-weight: 600;", "Linewise" }
            }

            // Tenant & Project selectors
            div {
                style: "padding: 12px; display: flex; flex-direction: column; gap: 8px;",

                label { style: "font-size: 11px; font-weight: 600; color: var(--text-secondary); text-transform: uppercase; letter-spacing: 0.5px;", "Organization" }
                crate::components::tenant_select::TenantSelector {}

                label { style: "font-size: 11px; font-weight: 600; color: var(--text-secondary); text-transform: uppercase; letter-spacing: 0.5px; margin-top: 4px;", "Project" }
                crate::components::project_select::ProjectSelector {}
            }

            // Spacer
            div { style: "flex: 1;" }

            // User info & sign out at bottom
            div {
                style: "padding: 12px; border-top: 1px solid var(--border);",
                div {
                    style: "font-size: 12px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; margin-bottom: 8px;",
                    "{user_email}"
                }
                SignOutButton {}
            }
        }
    }
}

#[component]
fn SignOutButton() -> Element {
    let services = use_context::<crate::state::CoreServices>();
    let app_state = use_context::<AppState>();
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

    rsx! {
        button {
            class: "btn-outline",
            style: "{styles::BTN_OUTLINE} width: 100%;",
            onclick: on_sign_out,
            "Sign Out"
        }
    }
}
