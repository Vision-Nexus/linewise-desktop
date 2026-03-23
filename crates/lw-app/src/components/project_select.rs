use crate::state::AppState;
use dioxus::prelude::*;

#[component]
pub fn ProjectSelector() -> Element {
    let mut app_state = use_context::<AppState>();
    let projects = app_state.projects.read();

    let selected_id = app_state
        .selected_project
        .read()
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_default();

    rsx! {
        select {
            style: "padding: 6px 8px; border: 1px solid #d1d5db; border-radius: 4px; font-size: 13px;",
            value: "{selected_id}",
            disabled: projects.is_empty(),
            onchange: move |evt| {
                let id = evt.value();
                if let Some(project) = app_state.projects.read().iter().find(|p| p.id == id) {
                    app_state.selected_project.set(Some(project.clone()));
                }
            },

            if projects.is_empty() {
                option { value: "", disabled: true, selected: true, "No projects" }
            } else {
                option { value: "", disabled: true, selected: selected_id.is_empty(), "Select Project" }
                for project in projects.iter() {
                    option {
                        key: "{project.id}",
                        value: "{project.id}",
                        selected: project.id == selected_id,
                        "{project.name}"
                    }
                }
            }
        }
    }
}
