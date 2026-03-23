use crate::state::AppState;
use dioxus::prelude::*;

#[component]
pub fn TenantSelector() -> Element {
    let mut app_state = use_context::<AppState>();
    let user_info = app_state.user_info.read();

    let tenants = user_info
        .as_ref()
        .map(|u| u.tenants.clone())
        .unwrap_or_default();

    let selected_name = app_state
        .selected_tenant
        .read()
        .as_ref()
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "Select Organization".to_string());

    rsx! {
        div { class: "tenant-selector",
            select {
                value: "{selected_name}",
                onchange: move |evt| {
                    let id = evt.value();
                    if let Some(ref info) = *app_state.user_info.read()
                        && let Some(tenant) = info.tenants.iter().find(|t| t.id == id)
                    {
                        app_state.selected_tenant.set(Some(tenant.clone()));
                    }
                },

                option { value: "", disabled: true, "Select Organization" }

                for tenant in tenants.iter() {
                    option {
                        key: "{tenant.id}",
                        value: "{tenant.id}",
                        "{tenant.name}"
                    }
                }
            }
        }
    }
}
