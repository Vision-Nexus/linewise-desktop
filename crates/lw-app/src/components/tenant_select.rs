use crate::state::AppState;
use crate::styles;
use dioxus::prelude::*;

#[component]
pub fn TenantSelector() -> Element {
    let mut app_state = use_context::<AppState>();
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
            style: "{styles::SELECT} width: 100%;",
            value: "{selected_id}",
            onchange: move |evt| {
                let id = evt.value();
                if let Some(ref info) = *app_state.user_info.read()
                    && let Some(tenant) = info.tenants.iter().find(|t| t.id == id)
                {
                    // Project re-fetch is owned by the Sidebar's
                    // `selected_tenant` effect; this handler just updates
                    // the selection signals.
                    app_state.selected_tenant.set(Some(tenant.clone()));
                    app_state.selected_project.set(None);
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
