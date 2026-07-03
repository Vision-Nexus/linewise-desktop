//! Completed tab: terminal-success rows, each collapsible to a detail
//! panel with a "Locate in sidebar" action.
//!
//! A collapsed row shows filename + org/project + size. Clicking it toggles
//! an expanded detail (local path, size, org/project, an "Already exists"
//! note for reconciled duplicates) and a `[Locate in sidebar]` button that
//! sets the tenant/project selection so the sidebar highlights that
//! org/project. Locate changes the upload *target* only — it does NOT filter
//! the panel (the panel stays global unless the user opts into the
//! per-project chip).
//!
//! Reconciled duplicates surface here: the `DuplicateDetected` arm in
//! `upload_runtime.rs` maps a dup to `Completed` and tags `error_message`
//! with the dedup marker, which this view turns into an "Already exists"
//! badge instead of an error.

use super::rows::{SectionHeader, format_size};
use super::tabs::{CompletedTab, SubTabButton};
use crate::state::AppState;
use dioxus::prelude::*;
use lw_core::models::{Project, Tenant, UploadState, UploadTask};

/// Marker string written into `error_message` by the `DuplicateDetected`
/// reconcile so a deduped row reads as "already stored" rather than failed.
/// Kept in one place so the writer (event handler) and the reader (this
/// view) can't drift.
pub const ALREADY_EXISTS_MARKER: &str = "Already exists on server";

#[component]
pub fn CompletedList(tasks: Vec<UploadTask>) -> Element {
    // Which row is expanded. `None` = all collapsed. Local state — a fresh
    // mount starting collapsed is the right default.
    let mut expanded: Signal<Option<String>> = use_signal(|| None);
    // Secondary tab (All / Completed / Already exists). Local state.
    let mut sub_tab = use_signal(|| CompletedTab::All);

    // Own the bucket filter, exactly like InProgressList / FailedList do. The
    // panel hands the full project-scoped list to every tab, so without this
    // the Completed tab would render in-progress and failed rows as completed.
    let completed: Vec<UploadTask> = tasks
        .iter()
        .filter(|t| t.state == UploadState::Completed)
        .cloned()
        .collect();
    let already_exists_count = completed
        .iter()
        .filter(|t| t.error_message.as_deref() == Some(ALREADY_EXISTS_MARKER))
        .count();
    let uploaded_count = completed.len() - already_exists_count;

    let active = *sub_tab.read();
    let shown: Vec<UploadTask> = completed
        .iter()
        .filter(|t| {
            let ae = t.error_message.as_deref() == Some(ALREADY_EXISTS_MARKER);
            match active {
                CompletedTab::All => true,
                CompletedTab::Uploaded => !ae,
                CompletedTab::AlreadyExists => ae,
            }
        })
        .cloned()
        .collect();

    // Section title / empty copy per sub-tab (prototype E3): the "Completed"
    // sub-tab's section reads "Uploaded"; "All" reads "Completed".
    let (section_title, empty_msg) = match active {
        CompletedTab::All => ("Completed", "Nothing completed yet"),
        CompletedTab::Uploaded => ("Uploaded", "Nothing uploaded yet"),
        CompletedTab::AlreadyExists => ("Already exists", "No files were already on the server"),
    };

    rsx! {
        if !completed.is_empty() {
            div {
                style: "display: flex; gap: 8px; margin-bottom: 16px;",
                SubTabButton {
                    label: "All".to_string(),
                    count: completed.len(),
                    active: active == CompletedTab::All,
                    onclick: move |_| sub_tab.set(CompletedTab::All),
                }
                SubTabButton {
                    label: "Completed".to_string(),
                    count: uploaded_count,
                    active: active == CompletedTab::Uploaded,
                    onclick: move |_| sub_tab.set(CompletedTab::Uploaded),
                }
                SubTabButton {
                    label: "Already exists".to_string(),
                    count: already_exists_count,
                    active: active == CompletedTab::AlreadyExists,
                    onclick: move |_| sub_tab.set(CompletedTab::AlreadyExists),
                }
            }
        }

        if shown.is_empty() {
            div {
                style: "text-align: center; padding: 40px 16px; color: var(--text-muted); font-size: 13px;",
                "{empty_msg}"
            }
        } else {
            SectionHeader { title: section_title.to_string(), count: shown.len() }
            div { style: "display: flex; flex-direction: column; gap: 6px;",
                for task in shown.iter() {
                    CompletedRow {
                        key: "{task.id}",
                        task: task.clone(),
                        is_expanded: expanded.read().as_deref() == Some(task.id.as_str()),
                        on_toggle: move |id: String| {
                            let open = expanded.read().as_deref() == Some(id.as_str());
                            expanded.set(if open { None } else { Some(id) });
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn CompletedRow(task: UploadTask, is_expanded: bool, on_toggle: EventHandler<String>) -> Element {
    let app_state = use_context::<AppState>();
    let tenant_name = app_state.tenant_display_name(&task.tenant_id);
    let project_name = app_state.project_display_name(&task.tenant_id, &task.project_id);
    let already_exists = task.error_message.as_deref() == Some(ALREADY_EXISTS_MARKER);

    let toggle_id = task.id.clone();
    let chevron = if is_expanded { "▾" } else { "▸" };

    rsx! {
        div {
            class: "card-row",
            style: "padding: 10px 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--success-bg); transition: background 0.15s, border-color 0.15s;",

            // Collapsed header — click toggles detail.
            div {
                style: "display: flex; justify-content: space-between; align-items: center; cursor: pointer;",
                onclick: move |_| on_toggle.call(toggle_id.clone()),
                div {
                    style: "flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px;",
                    span { style: "font-size: 12px; color: var(--text-muted); flex-shrink: 0;", "{chevron}" }
                    div {
                        style: "flex: 1; min-width: 0;",
                        span {
                            style: "font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block;",
                            "{task.filename}"
                        }
                        span {
                            style: "font-size: 11px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; display: block; margin-top: 2px;",
                            "{tenant_name} / {project_name}"
                        }
                    }
                }
                div {
                    style: "display: flex; align-items: center; gap: 8px; margin-left: 8px; flex-shrink: 0;",
                    if already_exists {
                        span {
                            style: "font-size: 10px; font-weight: 600; letter-spacing: 0.04em; text-transform: uppercase; padding: 2px 6px; border-radius: 10px; background: var(--bg-tertiary); color: var(--text-secondary);",
                            title: "This content already existed on the server — nothing new was uploaded.",
                            "Already exists"
                        }
                    } else {
                        span {
                            style: "font-size: 11px; color: var(--success); font-weight: 600; text-transform: uppercase;",
                            "Completed"
                        }
                    }
                    span { style: "font-size: 12px; color: var(--text-muted);", "{format_size(task.size)}" }
                }
            }

            if is_expanded {
                CompletedDetail {
                    task: task.clone(),
                    tenant_name: tenant_name.clone(),
                    project_name: project_name.clone(),
                    already_exists,
                }
            }
        }
    }
}

#[component]
fn CompletedDetail(
    task: UploadTask,
    tenant_name: String,
    project_name: String,
    already_exists: bool,
) -> Element {
    let mut app_state = use_context::<AppState>();
    let tenant_id = task.tenant_id.clone();
    let project_id = task.project_id.clone();

    // Locate: set tenant + project selection so the sidebar highlights this
    // org/project. We resolve the full `Tenant` / `Project` from the caches
    // because the selection signals hold the objects, not bare ids. If a
    // cache miss leaves us unable to build them, the click is a no-op rather
    // than clearing a good selection.
    let on_locate = move |_| {
        let tenant = locate_tenant(&app_state, &tenant_id);
        let project = locate_project(&app_state, &tenant_id, &project_id);
        if let Some(tenant) = tenant {
            app_state.selected_tenant.set(Some(tenant));
            app_state.selected_project.set(project);
        }
    };

    let detail_style = "display: grid; grid-template-columns: max-content 1fr; column-gap: 12px; row-gap: 4px; font-size: 12px; margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--border);";

    rsx! {
        div {
            style: "{detail_style}",
            div { style: "color: var(--text-muted);", "Org / Project" }
            div { style: "color: var(--text); word-break: break-all;", "{tenant_name} / {project_name}" }
            div { style: "color: var(--text-muted);", "Local path" }
            div { style: "color: var(--text); word-break: break-all; font-family: ui-monospace, SFMono-Regular, Menlo, monospace;", "{task.local_path}" }
            div { style: "color: var(--text-muted);", "Size" }
            div { style: "color: var(--text);", "{format_size(task.size)}" }
            if already_exists {
                div { style: "color: var(--text-muted);", "Note" }
                div { style: "color: var(--text-secondary);", "Content already existed on the server; no new upload was performed." }
            }
        }
        div {
            style: "display: flex; justify-content: flex-end; margin-top: 8px;",
            button {
                class: "btn-outline",
                style: "height: 26px; padding: 0 10px; font-size: 12px; border-radius: 6px; cursor: pointer; background: transparent; color: var(--text); border: 1px solid var(--border);",
                onclick: on_locate,
                "Locate in sidebar"
            }
        }
    }
}

/// Resolve a tenant id to the full `Tenant` from the `user_info` cache.
fn locate_tenant(app_state: &AppState, tenant_id: &str) -> Option<Tenant> {
    app_state
        .user_info
        .read()
        .as_ref()
        .and_then(|u| u.tenants.iter().find(|t| t.id == tenant_id).cloned())
}

/// Resolve a (tenant, project) id pair to the full `Project`. Searches the
/// per-tenant cache first, then the flat `projects` list. `None` when
/// neither has been hydrated — the caller then leaves the project unselected
/// (sidebar lands on the tenant's "All projects").
fn locate_project(app_state: &AppState, tenant_id: &str, project_id: &str) -> Option<Project> {
    if let Some(projects) = app_state.tenant_projects.read().get(tenant_id)
        && let Some(project) = projects.iter().find(|p| p.id == project_id)
    {
        return Some(project.clone());
    }
    app_state
        .projects
        .read()
        .iter()
        .find(|p| p.id == project_id)
        .cloned()
}
