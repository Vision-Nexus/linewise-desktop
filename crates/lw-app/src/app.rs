use crate::components::login::LoginPage;
use crate::components::upload_queue::UploadQueue;
use crate::state::AppState;
use dioxus::prelude::*;

#[component]
pub fn App() -> Element {
    use_context_provider(AppState::new);

    let app_state = use_context::<AppState>();
    let is_authenticated = *app_state.is_authenticated.read();

    if !is_authenticated {
        rsx! { LoginPage {} }
    } else {
        rsx! { MainView {} }
    }
}

#[component]
fn MainView() -> Element {
    let mut app_state = use_context::<AppState>();

    rsx! {
        div {
            style: "display: flex; flex-direction: column; height: 100vh;",

            header {
                style: "display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; border-bottom: 1px solid #e5e7eb; background: #f9fafb;",
                h1 { style: "margin: 0; font-size: 18px;", "Linewise Desktop" }
                div {
                    style: "display: flex; align-items: center; gap: 12px;",
                    crate::components::tenant_select::TenantSelector {}
                    button {
                        style: "padding: 6px 12px; border: 1px solid #ccc; border-radius: 4px; cursor: pointer; background: white;",
                        onclick: move |_| {
                            app_state.is_authenticated.set(false);
                            app_state.user_info.set(None);
                            app_state.selected_tenant.set(None);
                        },
                        "Sign Out"
                    }
                }
            }

            main {
                style: "flex: 1; overflow-y: auto;",
                UploadQueue {}
            }
        }
    }
}
