use crate::state::{AppState, CoreServices};
use crate::styles;
use dioxus::prelude::*;
use lw_core::api_client::ApiClient;
use lw_core::auth::AuthService;
use lw_core::error::AuthError;
use lw_core::models::AuthTokens;
use std::future::Future;
use std::sync::Arc;

/// Run `sign_in_future`, then fetch `/whoami`, then flip `AppState` into the
/// authenticated state. All three login paths (email, Google, Microsoft)
/// share this tail — only the initial credential step differs.
async fn complete_sign_in<F>(
    api: Arc<ApiClient>,
    mut app_state: AppState,
    mut error: Signal<Option<String>>,
    method: &'static str,
    sign_in_future: F,
) where
    F: Future<Output = Result<AuthTokens, AuthError>>,
{
    match sign_in_future.await {
        Ok(_tokens) => {
            tracing::info!("{method} sign-in succeeded");
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
        Err(AuthError::UserCancelled) => {
            tracing::info!("{method} sign-in cancelled by user");
        }
        Err(e) => {
            tracing::warn!("{method} sign-in failed: {e}");
            error.set(Some(e.to_string()));
        }
    }
}

#[component]
pub fn LoginPage() -> Element {
    let mut email = use_signal(String::new);
    let mut password = use_signal(String::new);
    let error = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| false);
    let app_state = use_context::<AppState>();
    let services = use_context::<CoreServices>();

    let run_sign_in = {
        let api = services.api.clone();
        let app_state = app_state.clone();
        let mut error = error;
        move |method: &'static str,
              sign_in: Box<dyn Future<Output = Result<AuthTokens, AuthError>> + Send>| {
            let api = api.clone();
            let app_state = app_state.clone();
            spawn(async move {
                loading.set(true);
                error.set(None);
                let fut = Box::into_pin(sign_in);
                complete_sign_in(api, app_state, error, method, fut).await;
                loading.set(false);
            });
        }
    };

    let on_submit = {
        let auth = services.auth.clone();
        let run_sign_in = run_sign_in.clone();
        move |evt: Event<FormData>| {
            evt.prevent_default();
            let email_val = email.read().clone();
            let password_val = password.read().clone();
            let auth = auth.clone();
            run_sign_in(
                "email",
                Box::new(async move { auth.sign_in_email(&email_val, &password_val).await }),
            );
        }
    };

    let on_google = {
        let auth = services.auth.clone();
        let run_sign_in = run_sign_in.clone();
        move |_| {
            let auth: Arc<AuthService> = auth.clone();
            run_sign_in(
                "google",
                Box::new(async move { auth.sign_in_google().await }),
            );
        }
    };

    let on_microsoft = {
        let auth = services.auth.clone();
        let run_sign_in = run_sign_in.clone();
        move |_| {
            let auth: Arc<AuthService> = auth.clone();
            run_sign_in(
                "microsoft",
                Box::new(async move { auth.sign_in_microsoft().await }),
            );
        }
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
                    disabled: *loading.read(),
                    onclick: on_google,
                    "Google"
                }
                button {
                    class: "btn-outline",
                    style: "{styles::BTN_OUTLINE}",
                    disabled: *loading.read(),
                    onclick: on_microsoft,
                    "Microsoft"
                }
            }
        }
    }
}
