use crate::components::network_status::NetworkStatusPill;
use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;
use lw_core::models::{UploadState, UploadTask};

/// Per-batch nav dot for a project row (mirrors the prototype `getBatchNavStatus`).
/// `None` = idle (no tasks) → no dot. Otherwise a coloured dot + tooltip; the
/// in-progress dot pulses.
struct NavDot {
    color: &'static str,
    pulse: bool,
    tooltip: String,
}

fn compute_nav_dot(tasks: &[UploadTask], tenant_id: &str, project_id: &str) -> Option<NavDot> {
    let (mut total, mut completed, mut failed, mut in_progress) = (0usize, 0usize, 0usize, 0usize);
    for t in tasks
        .iter()
        .filter(|t| t.tenant_id == tenant_id && t.project_id == project_id)
    {
        total += 1;
        match t.state {
            UploadState::Completed => completed += 1,
            UploadState::Failed | UploadState::GaveUp | UploadState::Rejected => failed += 1,
            UploadState::QualityChecking
            | UploadState::Hashing
            | UploadState::Staged
            | UploadState::Pending
            | UploadState::Validating
            | UploadState::Transcoding
            | UploadState::Creating
            | UploadState::Uploading
            | UploadState::Verifying
            | UploadState::Paused => in_progress += 1,
        }
    }
    if total == 0 {
        return None;
    }
    if in_progress > 0 {
        return Some(NavDot {
            color: "var(--info)",
            pulse: true,
            tooltip: format!("{completed} of {total} videos \u{00B7} {in_progress} in progress"),
        });
    }
    if failed > 0 {
        return Some(NavDot {
            color: "var(--error)",
            pulse: false,
            tooltip: format!("Batch complete \u{00B7} {completed} succeeded \u{00B7} {failed} failed"),
        });
    }
    Some(NavDot {
        color: "var(--success)",
        pulse: false,
        tooltip: format!("Batch complete \u{00B7} {completed} of {total} videos"),
    })
}

/// Group id used by the backend for vision-lab tenants. Matches
/// `parentGroupId` values returned in `TenantInfo`. Hardcoded here on
/// purpose: the desktop client tags this one group specifically; a
/// future second group would be added as a peer constant rather than
/// generalized.
const VISION_LAB_GROUP_ID: &str = "vision-lab";

/// Small "VL" pill rendered next to the display name for tenants in
/// the vision-lab group. Visual cue only — no behavioral effect.
#[component]
fn VisionLabBadge() -> Element {
    rsx! {
        span {
            class: "shrink-0 text-[10px] font-semibold tracking-wider \
                    px-1.5 py-0.5 rounded bg-primary/10 text-primary",
            title: "vision-lab tenant",
            "VL"
        }
    }
}

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

    // Re-fetch the selected tenant's projects whenever the org changes, so a
    // teammate creating or removing a project on the server shows up without
    // an app restart. The mount-time loop above seeds every tenant once;
    // this effect keeps the active tenant's slice fresh across switches.
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
                    // Keep the flat `projects` signal coherent when this
                    // tenant is the one currently selected — older readers
                    // still consult it for the active-tenant project list.
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

    // Real-time org search box state (filters the Orgs list below).
    let mut org_query = use_signal(String::new);

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

    // Live per-batch nav dots read the global task list — this subscribes the
    // sidebar so the dots update as uploads progress/complete/fail.
    let upload_tasks = app_state.upload_tasks.read();

    // Real-time filter: case-insensitive substring match on the org's display
    // name. Empty query → all orgs. Recomputed every keystroke (re-render).
    let org_query_lc = org_query.read().trim().to_lowercase();
    let filtered_tenants: Vec<_> = tenant_list
        .iter()
        .filter(|t| {
            org_query_lc.is_empty() || t.display_name.to_lowercase().contains(&org_query_lc)
        })
        .cloned()
        .collect();

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
                    class: "px-2 pt-2 shrink-0",
                    input {
                        r#type: "text",
                        value: "{org_query}",
                        placeholder: "Search orgs…",
                        spellcheck: "false",
                        autocapitalize: "off",
                        autocorrect: "off",
                        oninput: move |e| org_query.set(e.value()),
                        class: "w-full px-2 py-1.5 text-sm rounded border border-border bg-background text-foreground placeholder:text-muted-foreground focus:outline-none focus:border-ring",
                    }
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

                    for tenant in filtered_tenants.iter() {
                        {
                            let is_active = tenant.id == selected_tenant_id;
                            let active_class = if is_active {
                                "bg-primary/10 text-primary font-semibold"
                            } else {
                                "text-foreground hover:bg-accent"
                            };
                            let tenant_clone = tenant.clone();
                            let is_vision_lab = tenant.is_in_group(VISION_LAB_GROUP_ID);
                            rsx! {
                                div {
                                    key: "{tenant.id}",
                                    class: "px-3 py-2 mx-2 rounded cursor-pointer text-sm transition-colors flex items-center gap-1.5 min-w-0 {active_class}",
                                    onclick: move |_| {
                                        let changing = app_state.selected_tenant.read().as_ref().map(|t| t.id.clone()) != Some(tenant_clone.id.clone());
                                        app_state.selected_tenant.set(Some(tenant_clone.clone()));
                                        if changing {
                                            app_state.selected_project.set(None);
                                        }
                                    },
                                    span {
                                        class: "truncate min-w-0",
                                        "{tenant.display_name}"
                                    }
                                    if is_vision_lab {
                                        VisionLabBadge {}
                                    }
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
                    if !tenant_list.is_empty() && filtered_tenants.is_empty() {
                        div {
                            class: "px-3 py-2 text-sm text-muted-foreground italic",
                            "No matching orgs"
                        }
                    }
                }

                // Network status pill — sits at the bottom of the Orgs column
                // (always present), mapping the real 4-tier probe to Good/Slow/Offline.
                NetworkStatusPill {}
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
                                let nav = compute_nav_dot(&upload_tasks, &tenant.id, &project.id);
                                rsx! {
                                    div {
                                        key: "{project.id}",
                                        class: "px-4 py-2 mx-2 rounded cursor-pointer text-sm transition-colors flex items-center justify-between gap-1.5 {active_class}",
                                        onclick: move |_| {
                                            app_state.selected_project.set(Some(project.clone()));
                                            let projects = app_state.tenant_projects.read().get(&tenant.id).cloned().unwrap_or_default();
                                            app_state.projects.set(projects);
                                        },
                                        span { class: "truncate min-w-0", "{project.name}" }
                                        if let Some(dot) = nav {
                                            span {
                                                style: "position: relative; display: inline-flex; width: 8px; height: 8px; flex-shrink: 0;",
                                                title: "{dot.tooltip}",
                                                if dot.pulse {
                                                    span {
                                                        class: "lw-ping",
                                                        style: "position: absolute; inset: 0; border-radius: 999px; background: {dot.color};",
                                                    }
                                                }
                                                span {
                                                    style: "position: relative; width: 8px; height: 8px; border-radius: 999px; background: {dot.color};",
                                                }
                                            }
                                        }
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
