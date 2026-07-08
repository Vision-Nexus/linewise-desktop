//! Left sidebar — a single fixed-width column that mirrors the wave prototype's
//! drill-in navigation:
//!
//! * **Brand header** — the LineWise logo (click = back to the org list) plus
//!   the network status pill.
//! * **Body** — swaps entirely by selection: with no org selected it shows the
//!   organization picker (search + list of orgs with an initials tile and a
//!   batch count); once an org is selected it is *replaced* by that org's batch
//!   list, prefixed with an "All organizations" back link and the org name.
//!   Org and batch are never visible at once.
//! * **Profile footer** — avatar + name/email opening a menu with Settings,
//!   Repair… and Sign out.
//!
//! Only mounted after sign-in (inside `MainView`), so the per-tenant project
//! pre-fetch kicks off with a populated tenant list.

use crate::components::network_status::NetworkStatusPill;
use crate::components::transfer_panel::{NavDotView, NavScope, nav_status};
use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;
use lw_core::models::{Tenant, UploadTask};

/// Initials for an org's avatar tile — first letter of the first two words,
/// falling back to the first two characters.
fn org_initials(name: &str) -> String {
    let initials: String = name
        .split_whitespace()
        .take(2)
        .filter_map(|w| w.chars().next())
        .collect();
    if initials.is_empty() {
        name.chars().take(2).collect::<String>().to_uppercase()
    } else {
        initials.to_uppercase()
    }
}

#[component]
pub fn Sidebar() -> Element {
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    let tenants = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.tenants.clone())
        .unwrap_or_default();

    // Mount-time pre-fetch: hydrate every tenant's project list so the org
    // picker can show a batch count and the batch view is instant on open.
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

    // Re-fetch the selected tenant's projects whenever the org changes, so a
    // teammate creating or removing a project on the server shows up without
    // an app restart.
    let selected_tenant_id_for_refetch = app_state
        .selected_tenant
        .read()
        .as_ref()
        .map(|t| t.id.clone());
    let api_for_refetch = services.api.clone();
    let mut app_state_refetch = app_state.clone();
    use_effect(use_reactive!(|selected_tenant_id_for_refetch| {
        let Some(tenant_id) = selected_tenant_id_for_refetch.clone() else {
            return;
        };
        let api = api_for_refetch.clone();
        spawn(async move {
            match api.list_projects(&tenant_id).await {
                Ok(projects) => {
                    app_state_refetch
                        .tenant_projects
                        .write()
                        .insert(tenant_id.clone(), projects.clone());
                    let still_selected = app_state_refetch
                        .selected_tenant
                        .read()
                        .as_ref()
                        .map(|t| t.id == tenant_id)
                        .unwrap_or(false);
                    if still_selected {
                        app_state_refetch.projects.set(projects);
                    }
                }
                Err(e) => tracing::warn!(
                    tenant_id = %tenant_id,
                    "Project re-fetch on org switch failed: {e}"
                ),
            }
        });
    }));

    let selected_tenant = app_state.selected_tenant.read().clone();

    rsx! {
        aside {
            class: "w-[240px] h-full flex flex-col border-r border-border bg-background shrink-0",
            SidebarBrand {}
            if let Some(tenant) = selected_tenant {
                BatchList { tenant }
            } else {
                OrgList {}
            }
            ProfileFooter {}
        }
    }
}

/// Brand header: logo (returns to the org list) + network status pill.
#[component]
fn SidebarBrand() -> Element {
    let mut selected_tenant = use_context::<AppState>().selected_tenant;
    let mut selected_project = use_context::<AppState>().selected_project;
    rsx! {
        div {
            class: "shrink-0 border-b border-border px-3 py-3 flex items-center justify-between gap-2",
            button {
                class: "inline-flex items-center bg-transparent border-none cursor-pointer p-0 rounded hover:opacity-80",
                aria_label: "Back to organizations",
                onclick: move |_| {
                    selected_tenant.set(None);
                    selected_project.set(None);
                },
                crate::icons::LinewiseLogo { width: "96" }
            }
            NetworkStatusPill {}
        }
    }
}

/// Org picker (home state): search + list of orgs with an initials tile and a
/// batch count. Selecting an org drills into its batch list.
#[component]
fn OrgList() -> Element {
    let app_state = use_context::<AppState>();
    let mut selected_tenant = app_state.selected_tenant;
    let mut selected_project = app_state.selected_project;
    let mut org_query = use_signal(String::new);

    let tenants = app_state
        .user_info
        .read()
        .as_ref()
        .map(|u| u.tenants.clone())
        .unwrap_or_default();
    let total = tenants.len();
    let q = org_query.read().trim().to_lowercase();
    let filtered: Vec<_> = tenants
        .iter()
        .filter(|t| q.is_empty() || t.display_name.to_lowercase().contains(&q))
        .cloned()
        .collect();
    let count = filtered.len();
    let count_label = if total != count {
        format!("{count} of {total}")
    } else {
        format!("{count}")
    };
    let tenant_projects = app_state.tenant_projects.read();
    // Roll each org's tasks (across all its batches) up into a status dot.
    let upload_tasks = app_state.upload_tasks.read();

    rsx! {
        div {
            class: "shrink-0 px-3 pt-3 pb-2",
            div {
                class: "relative",
                span {
                    class: "pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-muted-foreground",
                    style: "display: inline-flex;",
                    crate::icons::SearchIcon {}
                }
                input {
                    r#type: "text",
                    value: "{org_query}",
                    placeholder: "Search…",
                    spellcheck: "false",
                    autocapitalize: "off",
                    autocorrect: "off",
                    oninput: move |e| org_query.set(e.value()),
                    class: "w-full h-8 pl-8 pr-2 text-sm rounded-md border border-border bg-background text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-ring",
                }
            }
        }

        div {
            class: "flex-1 overflow-y-auto px-2 pb-2",
            div {
                class: "flex items-center justify-between px-1 mb-2",
                span { class: "text-[11px] font-medium tracking-wide text-muted-foreground", "Organizations" }
                span { class: "text-[11px] tabular-nums text-muted-foreground", "{count_label}" }
            }

            for tenant in filtered.iter() {
                {
                    let t = tenant.clone();
                    let batches = tenant_projects.get(&tenant.id).map(|p| p.len()).unwrap_or(0);
                    let batch_label = if batches == 1 {
                        "1 batch".to_string()
                    } else {
                        format!("{batches} batches")
                    };
                    let initials = org_initials(&tenant.display_name);
                    // Roll this org's tasks (any batch) up into a trailing status dot.
                    let org_tasks: Vec<UploadTask> = upload_tasks
                        .iter()
                        .filter(|task| task.tenant_id == tenant.id)
                        .cloned()
                        .collect();
                    let nav = nav_status(&org_tasks, NavScope::Org);
                    rsx! {
                        button {
                            key: "{tenant.id}",
                            class: "w-full flex items-center gap-2.5 px-2.5 py-2 mb-1 rounded-lg text-left bg-transparent border-none cursor-pointer transition-colors hover:bg-accent",
                            onclick: move |_| {
                                selected_tenant.set(Some(t.clone()));
                                selected_project.set(None);
                            },
                            div {
                                class: "flex size-8 shrink-0 items-center justify-center rounded-md text-[11px] font-semibold bg-muted text-muted-foreground",
                                "{initials}"
                            }
                            div {
                                class: "min-w-0 flex-1",
                                p { class: "truncate text-[13px] leading-tight text-foreground", "{tenant.display_name}" }
                                p { class: "truncate text-[11px] leading-tight text-muted-foreground", "{batch_label}" }
                            }
                            if let Some(dot) = nav {
                                NavDotView { details: dot }
                            }
                        }
                    }
                }
            }

            if total == 0 {
                div { class: "px-3 py-6 text-center text-[13px] text-muted-foreground", "No organizations" }
            }
            if total > 0 && count == 0 {
                div { class: "px-3 py-6 text-center text-[13px] text-muted-foreground", "No matching organizations" }
            }
        }
    }
}

/// Batch list (org selected): back link + org title + the org's projects, each
/// with a folder icon and a trailing per-batch status dot.
#[component]
fn BatchList(tenant: Tenant) -> Element {
    let app_state = use_context::<AppState>();
    let mut selected_tenant = app_state.selected_tenant;
    let mut selected_project = app_state.selected_project;
    let mut projects_sig = app_state.projects;
    let tenant_projects_sig = app_state.tenant_projects;

    let projects = app_state
        .tenant_projects
        .read()
        .get(&tenant.id)
        .cloned()
        .unwrap_or_default();
    let selected_project_id = app_state
        .selected_project
        .read()
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_default();
    let upload_tasks = app_state.upload_tasks.read();
    let tid = tenant.id.clone();

    rsx! {
        div {
            class: "shrink-0 px-3 pt-3 pb-2",
            button {
                class: "w-full flex items-center gap-2 h-8 px-2 rounded-lg text-xs text-left text-muted-foreground bg-transparent border-none cursor-pointer hover:bg-accent hover:text-foreground",
                aria_label: "Back to organizations",
                onclick: move |_| {
                    selected_tenant.set(None);
                    selected_project.set(None);
                },
                crate::icons::ArrowLeftIcon {}
                span { "All organizations" }
            }
            h2 {
                class: "min-w-0 truncate px-1 pt-1 text-sm font-semibold tracking-tight text-foreground",
                "{tenant.display_name}"
            }
        }

        div {
            class: "flex-1 overflow-y-auto px-2 pb-2",
            div {
                class: "px-1 mb-1.5",
                span { class: "text-[11px] font-medium tracking-wide text-muted-foreground", "Batches" }
            }

            for project in projects.iter() {
                {
                    let p = project.clone();
                    let tid_click = tid.clone();
                    let is_active = project.id == selected_project_id;
                    let batch_tasks: Vec<UploadTask> = upload_tasks
                        .iter()
                        .filter(|task| task.tenant_id == tid && task.project_id == project.id)
                        .cloned()
                        .collect();
                    let nav = nav_status(&batch_tasks, NavScope::Batch);
                    let active_class = if is_active {
                        "bg-accent text-accent-foreground font-medium"
                    } else {
                        "text-foreground hover:bg-accent"
                    };
                    rsx! {
                        button {
                            key: "{project.id}",
                            class: "w-full flex items-center gap-2 h-9 px-3 mb-0.5 rounded-lg text-[13px] text-left bg-transparent border-none cursor-pointer transition-colors {active_class}",
                            onclick: move |_| {
                                selected_project.set(Some(p.clone()));
                                let projs = tenant_projects_sig.read().get(&tid_click).cloned().unwrap_or_default();
                                projects_sig.set(projs);
                            },
                            span {
                                class: "shrink-0 text-muted-foreground",
                                style: "display: inline-flex;",
                                crate::icons::FolderOpenIcon {}
                            }
                            span { class: "min-w-0 flex-1 truncate", "{project.name}" }
                            if let Some(dot) = nav {
                                NavDotView { details: dot }
                            }
                        }
                    }
                }
            }

            if projects.is_empty() {
                div { class: "px-3 py-6 text-center text-[13px] text-muted-foreground", "No batches" }
            }
        }
    }
}

/// Pinned account footer: avatar + name/email opening a menu with Settings,
/// Repair… and Sign out.
#[component]
fn ProfileFooter() -> Element {
    let app_state = use_context::<AppState>();
    let mut show_settings = app_state.show_settings;
    let mut show_repair = app_state.show_repair;
    let mut open = use_signal(|| false);

    let (name, email, photo) = {
        let ui = app_state.user_info.read();
        (
            ui.as_ref().and_then(|u| u.display_name.clone()),
            ui.as_ref().map(|u| u.email.clone()).unwrap_or_default(),
            ui.as_ref().and_then(|u| u.photo_url.clone()),
        )
    };
    let initial = name
        .as_deref()
        .or(Some(email.as_str()))
        .and_then(|s| s.chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    let display = name.clone().unwrap_or_else(|| email.clone());

    rsx! {
        div {
            class: "mt-auto shrink-0 border-t border-border p-2 relative",
            button {
                class: "w-full flex items-center gap-2 rounded-lg py-2.5 px-2 bg-transparent border-none cursor-pointer text-left hover:bg-accent",
                aria_label: "Open profile menu",
                onclick: move |_| {
                    let next = !*open.read();
                    open.set(next);
                },
                if let Some(url) = photo.clone() {
                    img {
                        src: "{url}",
                        alt: "",
                        class: "w-8 h-8 rounded-full object-cover shrink-0",
                        referrerpolicy: "no-referrer",
                    }
                } else {
                    div {
                        class: "w-8 h-8 rounded-full flex items-center justify-center bg-muted text-xs font-medium shrink-0 text-muted-foreground",
                        "{initial}"
                    }
                }
                div {
                    class: "grid min-w-0 flex-1 text-left leading-tight",
                    span { class: "truncate text-[13px] font-medium text-foreground", "{display}" }
                    span { class: "truncate text-[11px] text-muted-foreground", "{email}" }
                }
                span {
                    class: "ml-auto shrink-0 text-muted-foreground",
                    style: "display: inline-flex;",
                    crate::icons::ChevronsUpDownIcon {}
                }
            }

            if *open.read() {
                div { class: "fixed inset-0 z-40", onclick: move |_| open.set(false) }
                div {
                    class: "absolute left-2 right-2 bottom-full mb-1 z-50 bg-background border border-border rounded-md shadow-md p-1",
                    button {
                        class: "lw-menu-item",
                        onclick: move |_| {
                            open.set(false);
                            show_settings.set(true);
                        },
                        crate::icons::SettingsIcon {}
                        "Settings"
                    }
                    button {
                        class: "lw-menu-item",
                        onclick: move |_| {
                            open.set(false);
                            show_repair.set(true);
                        },
                        crate::icons::WrenchIcon {}
                        "Repair…"
                    }
                    div { class: "h-px bg-border my-1" }
                    SidebarSignOut { on_done: move |_| open.set(false) }
                }
            }
        }
    }
}

/// Sign-out menu item — mirrors the old title-bar sign-out: signs out via the
/// auth service and clears the session-scoped signals.
#[component]
fn SidebarSignOut(on_done: EventHandler<()>) -> Element {
    let app_state = use_context::<AppState>();
    let services = app_state.services.read().clone();
    let Some(services) = services else {
        return rsx! {};
    };
    let app_state_signout = app_state.clone();
    let mut signing_out = use_signal(|| false);

    let on_sign_out = move |_| {
        if *signing_out.read() {
            return;
        }
        signing_out.set(true);
        let auth = services.auth.clone();
        let mut app_state = app_state_signout.clone();
        // Do NOT close the menu here: `on_done` flips the parent `open` signal to
        // false, which unmounts THIS component's scope in the same event tick —
        // and Dioxus 0.7 drops the just-spawned future along with the scope, so
        // the sign-out body never runs (the observed "clicking Sign out does
        // nothing"). Flip auth state inside the task first (that alone swaps
        // MainView -> LoginPage and tears the sidebar down), then close the menu.
        spawn(async move {
            auth.sign_out().await;
            app_state.is_authenticated.set(false);
            app_state.user_info.set(None);
            app_state.selected_tenant.set(None);
            app_state.selected_project.set(None);
            app_state.projects.set(Vec::new());
            app_state.upload_tasks.set(Vec::new());
            on_done.call(());
        });
    };

    let is_busy = *signing_out.read();

    rsx! {
        button {
            class: "lw-menu-item is-destructive",
            disabled: is_busy,
            onclick: on_sign_out,
            if is_busy {
                span { class: "spinner spinner-sm" }
                "…"
            } else {
                crate::icons::LogoutIcon {}
                "Sign out"
            }
        }
    }
}
