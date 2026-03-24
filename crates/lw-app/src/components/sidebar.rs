use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::prelude::*;
use lw_core::models::{Project, Tenant};

#[component]
pub fn Sidebar() -> Element {
    let mut app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    let user_email = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_default();

    let tenants = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.tenants.clone())
        .unwrap_or_default();

    // Fetch projects for all tenants on mount
    let api = services.api.clone();
    let app_state_fetch = app_state.clone();
    use_future(move || {
        let api = api.clone();
        let tenants = tenants.clone();
        let mut app_state = app_state_fetch.clone();
        async move {
            for tenant in &tenants {
                match api.list_projects(&tenant.id).await {
                    Ok(projects) => {
                        app_state
                            .tenant_projects
                            .write()
                            .insert(tenant.id.clone(), projects);
                    }
                    Err(e) => tracing::warn!("Failed to fetch projects for {}: {e}", tenant.id),
                }
            }
        }
    });

    let selected_tenant_id = app_state
        .selected_tenant
        .read()
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_default();
    let selected_project_id = app_state
        .selected_project
        .read()
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_default();

    let tenant_list = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.tenants.clone())
        .unwrap_or_default();

    rsx! {
        aside {
            style: "width: {styles::SIDEBAR_WIDTH}px; height: 100vh; display: flex; flex-direction: column; border-right: 1px solid var(--border); background: var(--bg-secondary); flex-shrink: 0;",

            // App title
            div {
                style: "height: {styles::TOPBAR_HEIGHT}px; display: flex; align-items: center; padding: 0 16px; border-bottom: 1px solid var(--border); flex-shrink: 0;",
                h1 { style: "font-size: 15px; font-weight: 600;", "Linewise" }
            }

            // Tree nav — scrollable
            div {
                style: "flex: 1; overflow-y: auto; padding: 8px 0;",

                for tenant in tenant_list.iter() {
                    TenantNode {
                        key: "{tenant.id}",
                        tenant: tenant.clone(),
                        projects: app_state.tenant_projects.read().get(&tenant.id).cloned().unwrap_or_default(),
                        is_selected: tenant.id == selected_tenant_id,
                        selected_project_id: selected_project_id.clone(),
                        on_select_project: move |args: (Tenant, Project)| {
                            app_state.selected_tenant.set(Some(args.0.clone()));
                            app_state.selected_project.set(Some(args.1.clone()));
                            // Also update the flat projects list for upload queue
                            let projects = app_state.tenant_projects.read().get(&args.0.id).cloned().unwrap_or_default();
                            app_state.projects.set(projects);
                        },
                    }
                }

                if tenant_list.is_empty() {
                    div {
                        style: "padding: 16px; font-size: 13px; color: var(--text-muted);",
                        "No organizations"
                    }
                }
            }

            // User info & sign out
            div {
                style: "padding: 12px; border-top: 1px solid var(--border); flex-shrink: 0;",
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
fn TenantNode(
    tenant: Tenant,
    projects: Vec<Project>,
    is_selected: bool,
    selected_project_id: String,
    on_select_project: EventHandler<(Tenant, Project)>,
) -> Element {
    let mut expanded = use_signal(|| true);

    let toggle = move |_| {
        let current = *expanded.read();
        expanded.set(!current);
    };

    let is_open = *expanded.read();
    let arrow_class = if is_open { "collapse-arrow open" } else { "collapse-arrow" };

    rsx! {
        div {
            // Tenant header
            div {
                class: "card-row",
                style: "display: flex; align-items: center; gap: 6px; padding: 6px 12px; cursor: pointer; font-size: 13px; font-weight: 600; color: var(--text); transition: background 0.12s;",
                onclick: toggle,
                span { class: "{arrow_class}", style: "font-size: 10px; color: var(--text-muted); width: 12px;", "▸" }
                span { "{tenant.display_name}" }
                span {
                    style: "font-size: 11px; color: var(--text-muted); font-weight: 400; margin-left: auto;",
                    "{projects.len()}"
                }
            }

            // Project list
            if is_open {
                div {
                    style: "padding-left: 8px;",
                    for (idx, project) in projects.iter().enumerate() {
                        {
                            let is_active = project.id == selected_project_id;
                            let bg = if is_active { "var(--info-bg)" } else { "transparent" };
                            let color = if is_active { "var(--info)" } else { "var(--text-secondary)" };
                            let font_weight = if is_active { "600" } else { "400" };
                            let delay = idx * 30;
                            let tenant = tenant.clone();
                            let project = project.clone();
                            rsx! {
                                div {
                                    key: "{project.id}",
                                    class: "card-row stagger",
                                    style: "display: flex; align-items: center; padding: 5px 12px 5px 22px; cursor: pointer; font-size: 13px; color: {color}; font-weight: {font_weight}; background: {bg}; border-radius: 4px; margin: 1px 4px; transition: background 0.12s, color 0.12s; animation-delay: {delay}ms;",
                                    onclick: move |_| on_select_project.call((tenant.clone(), project.clone())),
                                    "{project.name}"
                                }
                            }
                        }
                    }
                    if projects.is_empty() {
                        div {
                            style: "padding: 4px 12px 4px 22px; font-size: 12px; color: var(--text-muted); font-style: italic;",
                            "No projects"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SignOutButton() -> Element {
    let services = use_context::<CoreServices>();
    let app_state = use_context::<AppState>();
    let app_state_signout = app_state.clone();
    let mut signing_out = use_signal(|| false);

    let on_sign_out = move |_| {
        if *signing_out.read() {
            return;
        }
        signing_out.set(true);
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

    let is_busy = *signing_out.read();

    rsx! {
        button {
            class: "btn-outline",
            style: "{styles::BTN_OUTLINE} width: 100%;",
            onclick: on_sign_out,
            disabled: is_busy,
            if is_busy {
                span { class: "spinner spinner-sm", style: "margin-right: 6px;" }
                "Signing out..."
            } else {
                "Sign Out"
            }
        }
    }
}
