use crate::state::{AppState, CoreServices};
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
        Ok(tokens) => {
            tracing::info!("{method} sign-in succeeded");
            let system_roles =
                lw_core::auth::claims::decode_unverified(&tokens.id_token).system_roles;
            match api.whoami().await {
                Ok(resp) => {
                    if let Some(info) = lw_core::models::UserInfo::from_whoami(resp, system_roles) {
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
    let mut show_email_form = use_signal(|| false);
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
            class: "flex h-full w-full",

            // Left panel — login form
            main {
                class: "flex flex-1 flex-col items-center justify-center p-10 bg-background",

                div {
                    class: "flex flex-col items-start gap-7 w-full min-w-[300px] max-w-[380px]",

                    // Logo
                    crate::icons::LinewiseLogo { width: "160" }

                    // Header
                    div {
                        class: "flex flex-col gap-1 w-full",
                        h1 {
                            class: "text-[22px] font-semibold text-foreground",
                            "Welcome to Linewise"
                        }
                        span {
                            class: "text-[15px] text-muted-foreground font-normal",
                            "Sign in to your account to continue"
                        }
                    }

                    // Error alert
                    if let Some(err) = error.read().as_ref() {
                        div {
                            class: "fade-in w-full px-3.5 py-2.5 bg-destructive-light border border-destructive rounded-md text-destructive text-[13px]",
                            "{err}"
                        }
                    }

                    // Auth buttons
                    div {
                        class: "w-full flex flex-col gap-3",

                        // Google
                        button {
                            class: "btn-social flex items-center justify-center gap-2.5 w-full h-[42px] px-4 border border-border rounded-lg text-sm font-medium bg-background text-foreground transition ease-out hover:bg-secondary",
                            disabled: *loading.read(),
                            onclick: on_google,
                            crate::icons::GoogleIcon {}
                            "Sign in with Google"
                        }

                        // Microsoft
                        button {
                            class: "btn-social flex items-center justify-center gap-2.5 w-full h-[42px] px-4 border border-border rounded-lg text-sm font-medium bg-background text-foreground transition ease-out hover:bg-secondary",
                            disabled: *loading.read(),
                            onclick: on_microsoft,
                            crate::icons::MicrosoftIcon {}
                            "Sign in with Microsoft"
                        }

                        // Separator
                        div {
                            class: "login-separator text-muted-foreground text-[13px]",
                            "or"
                        }

                        // Email form (toggle)
                        if *show_email_form.read() {
                            form {
                                onsubmit: on_submit,
                                class: "fade-in flex flex-col gap-3 w-full",

                                input {
                                    r#type: "email",
                                    placeholder: "Email",
                                    value: "{email}",
                                    oninput: move |evt| email.set(evt.value()),
                                    required: true,
                                    class: "h-[38px] px-3 border border-input rounded-md text-sm bg-background text-foreground outline-none transition focus:border-ring focus:shadow-[0_0_0_2px_rgba(103,19,219,0.15)]",
                                }

                                input {
                                    r#type: "password",
                                    placeholder: "Password",
                                    value: "{password}",
                                    oninput: move |evt| password.set(evt.value()),
                                    required: true,
                                    class: "h-[38px] px-3 border border-input rounded-md text-sm bg-background text-foreground outline-none transition focus:border-ring focus:shadow-[0_0_0_2px_rgba(103,19,219,0.15)]",
                                }

                                button {
                                    r#type: "submit",
                                    disabled: *loading.read(),
                                    class: "flex items-center justify-center w-full h-[42px] px-4 bg-primary text-primary-foreground rounded-lg text-sm font-medium transition ease-out hover:bg-primary-hovered active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed",
                                    if *loading.read() {
                                        span { class: "spinner spinner-sm mr-1.5" }
                                        "Signing in..."
                                    } else {
                                        "Sign in"
                                    }
                                }
                            }
                        } else {
                            button {
                                disabled: *loading.read(),
                                onclick: move |_| show_email_form.set(true),
                                class: "w-full h-[42px] px-4 bg-transparent text-muted-foreground rounded-lg text-sm font-medium transition ease-out hover:bg-accent hover:text-accent-foreground",
                                "Sign in with Email"
                            }
                        }
                    }

                    // Footer
                    p {
                        class: "text-[13px] text-muted-foreground text-center w-full",
                        "Don\u{2019}t have an account? Contact your administrator to get invited."
                    }
                }
            }

            // Right panel — background image
            div {
                class: "flex-1 h-full overflow-hidden",
                img {
                    src: "localasset://localhost/login-img.png",
                    alt: "Login Background",
                    class: "w-full h-full object-cover",
                }
            }
        }
    }
}
