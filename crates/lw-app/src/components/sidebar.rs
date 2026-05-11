use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;

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

    let user_display_name = app_state
        .user_info
        .read()
        .as_ref()
        .and_then(|u| u.display_name.clone());

    let user_photo_url = app_state
        .user_info
        .read()
        .as_ref()
        .and_then(|u| u.photo_url.clone());

    let avatar_initial = user_display_name
        .as_deref()
        .or(Some(user_email.as_str()))
        .and_then(|s| s.chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();

    let mut show_user_menu = use_signal(|| false);

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
            class: "flex h-screen shrink-0",

            // Left column — Logo + Tenants + User
            aside {
                class: "w-[200px] h-screen flex flex-col border-r border-border bg-background shrink-0",

                // Logo
                div {
                    class: "h-14 flex items-center justify-center border-b border-border shrink-0",
                    crate::icons::LinewiseLogo { width: "120" }
                }

                // Tenant list
                div {
                    class: "flex-1 overflow-y-auto py-3",
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
                                    class: "px-3 py-2.5 mx-2 cursor-pointer text-sm transition-colors truncate {active_class}",
                                    onclick: move |_| {
                                        app_state.selected_tenant.set(Some(tenant_clone.clone()));
                                    },
                                    "{tenant.display_name}"
                                }
                            }
                        }
                    }

                    if tenant_list.is_empty() {
                        div {
                            class: "px-3 py-2.5 text-sm text-muted-foreground italic",
                            "No orgs"
                        }
                    }
                }

                // User row: avatar-button + gear-button, with Sign Out popover
                div {
                    class: "border-t border-border shrink-0 relative",

                    div {
                        class: "flex items-center gap-1 p-1.5",

                        button {
                            class: "flex-1 min-w-0 flex items-center gap-2 rounded hover:bg-accent transition cursor-pointer text-left appearance-none border-none bg-transparent p-0",
                            onclick: move |_| {
                                let current = *show_user_menu.read();
                                show_user_menu.set(!current);
                            },

                            if let Some(url) = &user_photo_url {
                                img {
                                    src: "{url}",
                                    alt: "User avatar",
                                    class: "w-10 h-10 min-w-10 shrink-0 rounded-full object-cover",
                                    referrerpolicy: "no-referrer",
                                }
                            } else {
                                div {
                                    class: "w-10 h-10 min-w-10 shrink-0 rounded-full flex items-center justify-center bg-primary/10 text-primary text-base font-semibold",
                                    "{avatar_initial}"
                                }
                            }

                            div {
                                class: "flex flex-col items-start min-w-0 overflow-hidden",
                                if let Some(name) = &user_display_name {
                                    div {
                                        class: "text-[12px] text-foreground font-medium truncate",
                                        "{name}"
                                    }
                                }
                                div {
                                    class: "text-[11px] text-muted-foreground truncate",
                                    "{user_email}"
                                }
                            }
                        }

                        button {
                            class: "w-8 h-8 shrink-0 rounded hover:bg-accent transition cursor-pointer flex items-center justify-center appearance-none border-none bg-transparent text-muted-foreground",
                            title: "Settings",
                            aria_label: "Open settings",
                            onclick: move |_| app_state.show_settings.set(true),
                            crate::icons::SettingsIcon {}
                        }
                    }

                    if *show_user_menu.read() {
                        div {
                            class: "fixed inset-0 z-40",
                            onclick: move |_| show_user_menu.set(false),
                        }
                        div {
                            class: "absolute bottom-full left-0 right-0 mb-1 z-50 p-1 bg-background border border-border rounded-lg shadow-md",
                            onclick: move |e| e.stop_propagation(),

                            SignOutButton {}
                        }
                    }
                }
            }

            // Right column — Projects (only renders when tenant is selected)
            if selected_tenant.is_some() {
                div {
                    class: "w-[200px] min-w-[200px] max-w-[200px] h-screen flex flex-col border-r border-border bg-background shrink-0",

                    // Header
                    div {
                        class: "h-14 flex items-center px-4 border-b border-border shrink-0",
                        span { class: "text-base font-semibold text-foreground", "Projects" }
                    }

                    // Project list
                    div {
                        class: "flex-1 overflow-y-auto py-3",
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
                                        class: "px-4 py-2.5 mx-2 cursor-pointer text-sm transition-colors {active_class}",
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
                                class: "px-4 py-2.5 text-sm text-muted-foreground italic",
                                "No projects"
                            }
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
            class: "w-full h-9 px-3 text-sm rounded text-destructive bg-transparent transition ease-out hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed flex items-center",
            style: "gap: 8px;",
            onclick: on_sign_out,
            disabled: is_busy,
            if is_busy {
                span { class: "spinner spinner-sm mr-1" }
                "..."
            } else {
                crate::icons::LogoutIcon {}
                "Sign Out"
            }
        }
    }
}
