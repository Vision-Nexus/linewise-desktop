use crate::components::transfer_panel::stage_folder;
use crate::state::{AppState, CoreServices};
use dioxus::prelude::*;
use std::path::PathBuf;

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

    // Folder-picker launcher shared by the projects-panel Upload button and the
    // right-click project row (方案乙: the click/right-click ingest gesture is
    // "pick a folder of clips"). Given an explicit (tenant, project) target it
    // opens `rfd::pick_folder`, recurses for videos off the UI thread, and
    // stages each via the shared `stage_folder` path. Taking the target as
    // arguments keeps it unambiguous: the right-click handler sets the row as
    // the selection first, then passes that same row's ids here.
    let engine_for_picker = services.upload_engine.clone();
    let app_state_for_picker = app_state.clone();
    let launch_folder_picker = move |tenant_id: String, project_id: String| {
        let engine = engine_for_picker.clone();
        let app_state_toast = app_state_for_picker.clone();
        spawn(async move {
            let folder = rfd::AsyncFileDialog::new()
                .set_title("Select a folder of videos to upload")
                .pick_folder()
                .await;
            let Some(folder) = folder else { return };
            stage_folder(
                engine,
                PathBuf::from(folder.path()),
                tenant_id,
                project_id,
                app_state_toast,
            )
            .await;
        });
    };

    // The projects-panel Upload button targets the currently selected project;
    // it's only enabled when one is selected.
    let selected_project = app_state.selected_project.read().clone();
    let on_upload_click = {
        let launch = launch_folder_picker.clone();
        let selected_tenant = selected_tenant.clone();
        let selected_project = selected_project.clone();
        move |_| {
            let (Some(tenant), Some(project)) =
                (selected_tenant.as_ref(), selected_project.as_ref())
            else {
                return;
            };
            launch(tenant.id.clone(), project.id.clone());
        }
    };
    let upload_enabled = selected_project.is_some();

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
                }
            }

            if selected_tenant.is_some() {
                aside {
                    class: "w-[220px] min-w-[220px] max-w-[220px] h-full flex flex-col border-r border-border bg-background shrink-0",

                    div {
                        class: "h-10 flex items-center justify-between px-4 border-b border-border shrink-0 gap-2",
                        span { class: "text-xs font-semibold text-muted-foreground uppercase tracking-wider", "Projects" }
                        // Upload-to-selected-project button. Enabled only with a
                        // project selected; opens a folder picker (方案乙).
                        if upload_enabled {
                            button {
                                class: "shrink-0 text-[11px] font-medium px-2 py-1 rounded \
                                        bg-primary text-primary-foreground hover:bg-primary/90 \
                                        transition-colors cursor-pointer",
                                title: "Upload a folder of videos to the selected project",
                                onclick: on_upload_click,
                                "Upload"
                            }
                        } else {
                            button {
                                class: "shrink-0 text-[11px] font-medium px-2 py-1 rounded \
                                        bg-muted text-muted-foreground cursor-not-allowed",
                                disabled: true,
                                title: "Select a project to enable uploading",
                                "Upload"
                            }
                        }
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
                                // Separate clones for the right-click handler so
                                // both closures own their captures.
                                let tenant_ctx = tenant.clone();
                                let project_ctx = project.clone();
                                let launch_ctx = launch_folder_picker.clone();
                                let mut app_state_ctx = app_state.clone();
                                rsx! {
                                    div {
                                        key: "{project.id}",
                                        class: "px-4 py-2 mx-2 rounded cursor-pointer text-sm transition-colors {active_class}",
                                        onclick: move |_| {
                                            app_state.selected_project.set(Some(project.clone()));
                                            let projects = app_state.tenant_projects.read().get(&tenant.id).cloned().unwrap_or_default();
                                            app_state.projects.set(projects);
                                        },
                                        // Right-click a project row → make it the
                                        // selection, then open the folder picker
                                        // for THAT project (方案乙, no menu). The
                                        // `prevent_default` suppresses the Windows
                                        // webview's native context menu so only
                                        // our picker opens.
                                        oncontextmenu: move |e: Event<MouseData>| {
                                            e.prevent_default();
                                            app_state_ctx.selected_tenant.set(Some(tenant_ctx.clone()));
                                            app_state_ctx.selected_project.set(Some(project_ctx.clone()));
                                            let projects = app_state_ctx.tenant_projects.read().get(&tenant_ctx.id).cloned().unwrap_or_default();
                                            app_state_ctx.projects.set(projects);
                                            launch_ctx(tenant_ctx.id.clone(), project_ctx.id.clone());
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
