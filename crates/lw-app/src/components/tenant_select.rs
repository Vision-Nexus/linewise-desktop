use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::prelude::*;

#[component]
pub fn TenantSelector() -> Element {
    let mut app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();
    let user_info = app_state.user_info.read();

    let tenants = user_info
        .as_ref()
        .map(|u| u.tenants.clone())
        .unwrap_or_default();

    let selected_id = app_state
        .selected_tenant
        .read()
        .as_ref()
        .map(|t| t.id.clone())
        .unwrap_or_default();

    rsx! {
        select {
            style: "{styles::SELECT}",
            value: "{selected_id}",
            onchange: move |evt| {
                let id = evt.value();
                let api = services.api.clone();
                if let Some(ref info) = *app_state.user_info.read()
                    && let Some(tenant) = info.tenants.iter().find(|t| t.id == id)
                {
                    let tenant = tenant.clone();
                    app_state.selected_tenant.set(Some(tenant.clone()));
                    app_state.selected_project.set(None);
                    let tenant_id = tenant.id.clone();
                    spawn(async move {
                        match api.list_projects(&tenant_id).await {
                            Ok(projects) => app_state.projects.set(projects),
                            Err(e) => tracing::warn!("Failed to fetch projects: {e}"),
                        }
                    });
                }
            },

            option { value: "", disabled: true, selected: selected_id.is_empty(), "Select Organization" }

            for tenant in tenants.iter() {
                option {
                    key: "{tenant.id}",
                    value: "{tenant.id}",
                    selected: tenant.id == selected_id,
                    "{tenant.display_name}"
                }
            }
        }
    }
}
