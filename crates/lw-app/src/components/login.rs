use crate::state::{AppState, CoreServices};
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
            style: "display: flex; flex-direction: column; align-items: center; justify-content: center; height: 100vh; gap: 16px; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;",

            h1 { style: "margin: 0;", "Linewise Desktop" }
            p { style: "margin: 0; color: #666;", "Sign in to continue" }

            form {
                onsubmit: on_submit,
                style: "display: flex; flex-direction: column; gap: 12px; width: 320px;",

                input {
                    r#type: "email",
                    placeholder: "Email",
                    value: "{email}",
                    oninput: move |evt| email.set(evt.value()),
                    required: true,
                    style: "padding: 10px; border: 1px solid #d1d5db; border-radius: 6px; font-size: 14px; outline: none;",
                }

                input {
                    r#type: "password",
                    placeholder: "Password",
                    value: "{password}",
                    oninput: move |evt| password.set(evt.value()),
                    required: true,
                    style: "padding: 10px; border: 1px solid #d1d5db; border-radius: 6px; font-size: 14px; outline: none;",
                }

                button {
                    r#type: "submit",
                    disabled: *loading.read(),
                    style: "padding: 10px; background: #2563eb; color: white; border: none; border-radius: 6px; cursor: pointer; font-size: 14px; font-weight: 500;",
                    if *loading.read() { "Signing in..." } else { "Sign In" }
                }
            }

            if let Some(err) = error.read().as_ref() {
                p {
                    style: "color: #ef4444; font-size: 13px; max-width: 320px; text-align: center;",
                    "{err}"
                }
            }

            div {
                style: "margin-top: 8px; display: flex; gap: 8px;",
                button {
                    style: "padding: 8px 16px; border: 1px solid #d1d5db; border-radius: 6px; cursor: pointer; background: white; font-size: 13px;",
                    onclick: move |_| {
                        // TODO: OAuth via local HTTP redirect
                        tracing::info!("Google OAuth sign-in (not yet implemented)");
                    },
                    "Google"
                }
                button {
                    style: "padding: 8px 16px; border: 1px solid #d1d5db; border-radius: 6px; cursor: pointer; background: white; font-size: 13px;",
                    onclick: move |_| {
                        tracing::info!("Microsoft OAuth sign-in (not yet implemented)");
                    },
                    "Microsoft"
                }
            }
        }
    }
}
