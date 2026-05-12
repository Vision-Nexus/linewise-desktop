use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;

/// Left sidebar — tenant list in the first column, project list in the
/// second. Only renders after sign-in (mounted inside `MainView`), so the
/// per-tenant project pre-fetch kicks off with a populated tenant list.
///
/// Tenants and projects can number in the hundreds, so both columns scroll
/// vertically rather than relying on dropdowns from the title bar.
#[component]
pub fn Sidebar() -> Element {
    let mut app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    let tenants = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.tenants.clone())
        .unwrap_or_default();

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

    let selected_tenant = app_state.selected_tenant.read().clone();
    let selected_tenant_id = selected_tenant
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

    let projects_for_selected = selected_tenant
        .as_ref()
        .and_then(|t| app_state.tenant_projects.read().get(&t.id).cloned())
        .unwrap_or_default();

    rsx! {
        div {
            class: "flex h-full shrink-0",

            aside {
                class: "w-[200px] h-full flex flex-col border-r border-border bg-background shrink-0",

                div {
                    class: "h-10 flex items-center px-4 border-b border-border shrink-0",
                    span { class: "text-xs font-semibold text-muted-foreground uppercase tracking-wider", "Orgs" }
                }

                div {
                    class: "flex-1 overflow-y-auto py-2",

                    // Pseudo-entry: no tenant selected → queue shows all orgs.
                    {
                        let is_all = selected_tenant.is_none();
                        let active_class = if is_all {
                            "bg-primary/10 text-primary font-semibold"
                        } else {
                            "text-muted-foreground hover:bg-accent"
                        };
                        rsx! {
                            div {
                                class: "px-3 py-2 mx-2 rounded cursor-pointer text-sm transition-colors truncate {active_class}",
                                onclick: move |_| {
                                    app_state.selected_tenant.set(None);
                                    app_state.selected_project.set(None);
                                },
                                "All orgs"
                            }
                        }
                    }

                    for tenant in tenant_list.iter() {
                        {
                            let is_active = tenant.id == selected_tenant_id;
                            let active_class = if is_active {
                                "bg-primary/10 text-primary font-semibold"
                            } else {
                                "text-foreground hover:bg-accent"
                            };
                            let tenant_clone = tenant.clone();
                            rsx! {
                                div {
                                    key: "{tenant.id}",
                                    class: "px-3 py-2 mx-2 rounded cursor-pointer text-sm transition-colors truncate {active_class}",
                                    onclick: move |_| {
                                        let changing = app_state.selected_tenant.read().as_ref().map(|t| t.id.clone()) != Some(tenant_clone.id.clone());
                                        app_state.selected_tenant.set(Some(tenant_clone.clone()));
                                        if changing {
                                            app_state.selected_project.set(None);
                                        }
                                    },
                                    "{tenant.display_name}"
                                }
                            }
                        }
                    }

                    if tenant_list.is_empty() {
                        div {
                            class: "px-3 py-2 text-sm text-muted-foreground italic",
                            "No orgs"
                        }
                    }
                }
            }

            if selected_tenant.is_some() {
                aside {
                    class: "w-[220px] min-w-[220px] max-w-[220px] h-full flex flex-col border-r border-border bg-background shrink-0",

                    div {
                        class: "h-10 flex items-center px-4 border-b border-border shrink-0",
                        span { class: "text-xs font-semibold text-muted-foreground uppercase tracking-wider", "Projects" }
                    }

                    div {
                        class: "flex-1 overflow-y-auto py-2",

                        // Pseudo-entry: tenant selected, no project → queue
                        // shows every project in this tenant.
                        {
                            let is_all = app_state.selected_project.read().is_none();
                            let active_class = if is_all {
                                "bg-primary/10 text-primary font-medium border-l-2 border-primary"
                            } else {
                                "text-muted-foreground hover:bg-accent hover:text-foreground"
                            };
                            rsx! {
                                div {
                                    class: "px-4 py-2 mx-2 rounded cursor-pointer text-sm transition-colors {active_class}",
                                    onclick: move |_| {
                                        app_state.selected_project.set(None);
                                    },
                                    "All projects"
                                }
                            }
                        }

                        for project in projects_for_selected.iter() {
                            {
                                let is_active = project.id == selected_project_id;
                                let active_class = if is_active {
                                    "bg-primary/10 text-primary font-medium border-l-2 border-primary"
                                } else {
                                    "text-muted-foreground hover:bg-accent hover:text-foreground"
                                };
                                let tenant = selected_tenant.clone().expect("checked is_some above");
                                let project = project.clone();
                                rsx! {
                                    div {
                                        key: "{project.id}",
                                        class: "px-4 py-2 mx-2 rounded cursor-pointer text-sm transition-colors {active_class}",
                                        onclick: move |_| {
                                            app_state.selected_project.set(Some(project.clone()));
                                            let projects = app_state.tenant_projects.read().get(&tenant.id).cloned().unwrap_or_default();
                                            app_state.projects.set(projects);
                                        },
                                        "{project.name}"
                                    }
                                }
                            }
                        }

                        if projects_for_selected.is_empty() {
                            div {
                                class: "px-4 py-2 text-sm text-muted-foreground italic",
                                "No projects"
                            }
                        }
                    }
                }
            }
        }
    }
}
