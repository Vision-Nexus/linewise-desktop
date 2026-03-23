use crate::state::AppState;
use dioxus::prelude::*;

#[component]
pub fn LoginPage() -> Element {
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| false);
    let _app_state = use_context::<AppState>();

    let on_submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        let email_val = email.read().clone();
        let _password_val = password.read().clone();

        spawn(async move {
            loading.set(true);
            error.set(None);

            // TODO: Call auth service with email_val and password_val
            tracing::info!("Login attempt for: {email_val}");

            // On success: update state
            // app_state.is_authenticated.set(true);

            loading.set(false);
        });
    };

    rsx! {
        div {
            style: "display: flex; flex-direction: column; align-items: center; justify-content: center; height: 100vh; gap: 16px;",

            h1 { "Linewise Desktop" }
            p { "Sign in to continue" }

            form {
                onsubmit: on_submit,
                style: "display: flex; flex-direction: column; gap: 12px; width: 320px;",

                input {
                    r#type: "email",
                    placeholder: "Email",
                    value: "{email}",
                    oninput: move |evt| email.set(evt.value()),
                    required: true,
                    style: "padding: 8px; border: 1px solid #ccc; border-radius: 4px;",
                }

                input {
                    r#type: "password",
                    placeholder: "Password",
                    value: "{password}",
                    oninput: move |evt| password.set(evt.value()),
                    required: true,
                    style: "padding: 8px; border: 1px solid #ccc; border-radius: 4px;",
                }

                button {
                    r#type: "submit",
                    disabled: *loading.read(),
                    style: "padding: 10px; background: #2563eb; color: white; border: none; border-radius: 4px; cursor: pointer;",
                    if *loading.read() { "Signing in..." } else { "Sign In" }
                }
            }

            if let Some(err) = error.read().as_ref() {
                p {
                    style: "color: red; font-size: 14px;",
                    "{err}"
                }
            }

            div {
                style: "margin-top: 16px; display: flex; gap: 8px;",
                button {
                    style: "padding: 8px 16px; border: 1px solid #ccc; border-radius: 4px; cursor: pointer;",
                    onclick: move |_| {
                        tracing::info!("Google OAuth sign-in");
                    },
                    "Sign in with Google"
                }
                button {
                    style: "padding: 8px 16px; border: 1px solid #ccc; border-radius: 4px; cursor: pointer;",
                    onclick: move |_| {
                        tracing::info!("Microsoft OAuth sign-in");
                    },
                    "Sign in with Microsoft"
                }
            }
        }
    }
}
