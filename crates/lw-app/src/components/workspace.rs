//! The main content area, routed by the sidebar selection to mirror the wave
//! prototype's per-batch pages:
//!
//! * no org selected  → "Select an organization" empty state
//! * org, no batch    → the org landing: title + "Select a batch" (with folder
//!                      cards) or "No batches yet"
//! * org + batch      → the batch view: org/batch header + the (batch-scoped)
//!                      transfer panel
//!
//! Selection still flows through `AppState::selected_tenant` /
//! `selected_project`; this component just renders the matching view.

use crate::components::transfer_panel::TransferPanel;
use crate::state::AppState;
use dioxus::prelude::*;
use lw_core::models::{Project, Tenant};

#[component]
pub fn Workspace() -> Element {
    let app_state = use_context::<AppState>();
    let tenant = app_state.selected_tenant.read().clone();
    let project = app_state.selected_project.read().clone();

    match (tenant, project) {
        (None, _) => rsx! { SelectOrgEmpty {} },
        (Some(tenant), None) => rsx! { OrgLanding { tenant } },
        (Some(tenant), Some(project)) => rsx! { BatchView { tenant, project } },
    }
}

/// Shared empty-state shell (dashed bordered card, centered icon + title + desc).
#[component]
fn EmptyState(title: String, description: String, children: Element) -> Element {
    rsx! {
        div {
            class: "flex w-full min-w-0 flex-1 flex-col items-center justify-center gap-4 \
                    rounded-xl border border-dashed border-border p-6 text-center",
            div {
                class: "flex max-w-sm flex-col items-center gap-2",
                {children}
                div { class: "text-sm font-medium tracking-tight text-foreground", "{title}" }
                div { class: "text-sm text-muted-foreground", "{description}" }
            }
        }
    }
}

/// Rounded muted media tile that holds an empty-state icon.
#[component]
fn EmptyMedia(children: Element) -> Element {
    rsx! {
        div {
            class: "mb-2 flex size-8 items-center justify-center rounded-lg bg-muted text-foreground",
            {children}
        }
    }
}

#[component]
fn SelectOrgEmpty() -> Element {
    rsx! {
        div {
            class: "flex flex-1 flex-col p-6",
            EmptyState {
                title: "Select an organization".to_string(),
                description: "Choose an organization from the sidebar to view its batches and content.".to_string(),
                EmptyMedia { crate::icons::BuildingIcon { size: "16" } }
            }
        }
    }
}

#[component]
fn OrgLanding(tenant: Tenant) -> Element {
    let app_state = use_context::<AppState>();
    let projects = app_state
        .tenant_projects
        .read()
        .get(&tenant.id)
        .cloned()
        .unwrap_or_default();
    let n = projects.len();
    let batch_label = if n == 1 {
        "1 batch".to_string()
    } else {
        format!("{n} batches")
    };

    rsx! {
        div {
            class: "flex flex-1 flex-col gap-8 p-6",
            header {
                h1 { class: "text-2xl font-semibold tracking-tight text-foreground", "{tenant.display_name}" }
                p { class: "text-sm text-muted-foreground", "{batch_label}" }
            }

            if projects.is_empty() {
                EmptyState {
                    title: "No batches yet".to_string(),
                    description: "This organization does not have any batches.".to_string(),
                    EmptyMedia { crate::icons::FolderIcon { size: "16" } }
                }
            } else {
                div {
                    class: "flex w-full min-w-0 flex-1 flex-col items-center justify-center gap-4 \
                            rounded-xl border border-dashed border-border p-6 text-center",
                    div {
                        class: "flex max-w-sm flex-col items-center gap-2",
                        EmptyMedia { crate::icons::FolderIcon { size: "16" } }
                        div { class: "text-sm font-medium tracking-tight text-foreground", "Select a batch" }
                        div { class: "text-sm text-muted-foreground", "Pick a batch from the sidebar to open it, or choose one below." }
                    }
                    div {
                        class: "flex flex-row flex-wrap justify-center gap-5",
                        for project in projects.iter() {
                            BatchCard {
                                key: "{project.id}",
                                tenant_id: tenant.id.clone(),
                                project: project.clone(),
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn BatchCard(tenant_id: String, project: Project) -> Element {
    let app_state = use_context::<AppState>();
    let mut selected_project = app_state.selected_project;
    let mut projects_sig = app_state.projects;
    let tenant_projects_sig = app_state.tenant_projects;

    let description = project
        .description
        .clone()
        .filter(|d| !d.trim().is_empty());
    let p = project.clone();
    let tid = tenant_id.clone();

    rsx! {
        button {
            class: "group shrink-0 flex h-[132px] w-[148px] flex-col justify-between p-3.5 \
                    rounded-xl border border-border bg-background text-left cursor-pointer \
                    transition-colors hover:bg-accent",
            aria_label: "Open {project.name}",
            onclick: move |_| {
                selected_project.set(Some(p.clone()));
                let projs = tenant_projects_sig.read().get(&tid).cloned().unwrap_or_default();
                projects_sig.set(projs);
            },
            span {
                class: "text-muted-foreground group-hover:text-primary transition-colors",
                style: "display: inline-flex;",
                crate::icons::FolderIcon { size: "34" }
            }
            div {
                class: "w-full min-w-0",
                p { class: "m-0 w-full truncate text-sm font-medium leading-tight text-foreground", "{project.name}" }
                if let Some(desc) = description {
                    p { class: "m-0 mt-1 w-full truncate text-xs leading-tight text-muted-foreground", "{desc}" }
                }
            }
        }
    }
}

#[component]
fn BatchView(tenant: Tenant, project: Project) -> Element {
    let description = project
        .description
        .clone()
        .filter(|d| !d.trim().is_empty());
    rsx! {
        div {
            class: "flex min-h-0 flex-1 flex-col gap-4 p-4",
            header {
                class: "shrink-0",
                p { class: "text-sm text-muted-foreground truncate", "{tenant.display_name}" }
                h1 { class: "text-2xl font-semibold tracking-tight text-foreground truncate", "{project.name}" }
                if let Some(desc) = description {
                    p { class: "mt-1 text-sm text-muted-foreground truncate", "{desc}" }
                }
            }
            // The panel scopes itself to the selected batch (see TransferPanel).
            TransferPanel {}
        }
    }
}
