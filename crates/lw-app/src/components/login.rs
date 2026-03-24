use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::prelude::*;

#[component]
pub fn LoginPage() -> Element {
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| false);
    let mut app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        let email_val = email.read().clone();
        let password_val = password.read().clone();
        let auth = services.auth.clone();
        let api = services.api.clone();

        spawn(async move {
            loading.set(true);
            error.set(None);

            match auth.sign_in_email(&email_val, &password_val).await {
                Ok(_tokens) => {
                    tracing::info!("Login successful for: {email_val}");
                    match api.whoami().await {
                        Ok(resp) => {
                            if let Some(info) = lw_core::models::UserInfo::from_whoami(resp) {
                                app_state.user_info.set(Some(info));
                                app_state.is_authenticated.set(true);
                            } else {
                                error.set(Some("No user account found".to_string()));
                            }
                        }
                        Err(e) => {
                            error.set(Some(format!("Failed to fetch user info: {e}")));
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Login failed: {e}");
                    error.set(Some(e.to_string()));
                }
            }

            loading.set(false);
        });
    };

    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; justify-content: center; height: 100vh; gap: 20px;",

            h1 { style: "font-size: 22px; font-weight: 600;", "Linewise Desktop" }
            p { style: "color: #6b7280; font-size: 14px;", "Sign in to continue" }

            form {
                onsubmit: on_submit,
                style: "display: flex; flex-direction: column; gap: 12px; width: 320px;",

                input {
                    r#type: "email",
                    placeholder: "Email",
                    value: "{email}",
                    oninput: move |evt| email.set(evt.value()),
                    required: true,
                    style: "{styles::INPUT}",
                }

                input {
                    r#type: "password",
                    placeholder: "Password",
                    value: "{password}",
                    oninput: move |evt| password.set(evt.value()),
                    required: true,
                    style: "{styles::INPUT}",
                }

                button {
                    r#type: "submit",
                    class: "btn-primary",
                    disabled: *loading.read(),
                    style: "{styles::BTN_PRIMARY} width: 100%; height: 38px; font-size: 14px;",
                    if *loading.read() {
                        span { class: "spinner spinner-sm", style: "margin-right: 6px;" }
                        "Signing in..."
                    } else {
                        "Sign In"
                    }
                }
            }

            if let Some(err) = error.read().as_ref() {
                div {
                    class: "fade-in",
                    style: "max-width: 320px; padding: 10px 14px; background: var(--error-bg); border: 1px solid var(--error); border-radius: 6px; color: var(--error); font-size: 13px;",
                    "{err}"
                }
            }

            div {
                style: "margin-top: 4px; display: flex; gap: 8px;",
                button {
                    class: "btn-outline",
                    style: "{styles::BTN_OUTLINE}",
                    onclick: move |_| {
                        tracing::info!("Google OAuth sign-in (not yet implemented)");
                    },
                    "Google"
                }
                button {
                    class: "btn-outline",
                    style: "{styles::BTN_OUTLINE}",
                    onclick: move |_| {
                        tracing::info!("Microsoft OAuth sign-in (not yet implemented)");
                    },
                    "Microsoft"
                }
            }
        }
    }
}
