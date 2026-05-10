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
            class: "flex h-screen w-full",

            // Left panel — login form
            main {
                class: "flex flex-1 flex-col items-center justify-center p-10 bg-background",

                div {
                    class: "flex flex-col items-start gap-7 w-full min-w-[300px] max-w-[380px]",

                    // Logo
                    svg {
                        width: "50",
                        height: "40",
                        view_box: "0 0 75 60",
                        fill: "none",
                        xmlns: "http://www.w3.org/2000/svg",
                        path {
                            d: "M2.42566 25.7755C11.9699 32.1332 18.2561 42.9924 18.2561 55.3198H34.7938C34.7938 38.5216 26.8327 23.5848 14.4747 14.0763L2.42566 25.7755Z",
                            fill: "#20026E",
                        }
                        path {
                            d: "M18.2587 55.3224C18.2587 26.5952 41.5448 3.30908 70.2719 3.30908V19.8468C50.6779 19.8468 34.7964 35.7309 34.7964 55.3224H18.2587Z",
                            fill: "url(#paint0_linear_login)",
                        }
                        defs {
                            linearGradient {
                                id: "paint0_linear_login",
                                x1: "18.2587",
                                y1: "29.3144",
                                x2: "70.2719",
                                y2: "29.3144",
                                gradient_units: "userSpaceOnUse",
                                stop { stop_color: "#5C01DA" }
                                stop { offset: "1", stop_color: "#20026E" }
                            }
                        }
                    }

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
                            svg {
                                width: "20",
                                height: "20",
                                view_box: "0 0 20 20",
                                fill: "none",
                                xmlns: "http://www.w3.org/2000/svg",
                                path { d: "M19.6 10.2273C19.6 9.51818 19.5364 8.83636 19.4182 8.18182H10V12.05H15.3818C15.15 13.3 14.4455 14.3591 13.3864 15.0682V17.5773H16.6182C18.5091 15.8364 19.6 13.2727 19.6 10.2273Z", fill: "#4285F4" }
                                path { d: "M10 20C12.7 20 14.9636 19.1045 16.6181 17.5773L13.3863 15.0682C12.4909 15.6682 11.3454 16.0227 10 16.0227C7.39545 16.0227 5.19091 14.2636 4.40455 11.9H1.06364V14.4909C2.70909 17.7591 6.09091 20 10 20Z", fill: "#34A853" }
                                path { d: "M4.40455 11.9C4.20455 11.3 4.09091 10.6591 4.09091 10C4.09091 9.34091 4.20455 8.7 4.40455 8.1V5.50909H1.06364C0.386364 6.85909 0 8.38636 0 10C0 11.6136 0.386364 13.1409 1.06364 14.4909L4.40455 11.9Z", fill: "#FBBC04" }
                                path { d: "M10 3.97727C11.4681 3.97727 12.7863 4.48182 13.8227 5.47273L16.6909 2.60455C14.9591 0.990909 12.6954 0 10 0C6.09091 0 2.70909 2.24091 1.06364 5.50909L4.40455 8.1C5.19091 5.73636 7.39545 3.97727 10 3.97727Z", fill: "#E94235" }
                            }
                            "Sign in with Google"
                        }

                        // Microsoft
                        button {
                            class: "btn-social flex items-center justify-center gap-2.5 w-full h-[42px] px-4 border border-border rounded-lg text-sm font-medium bg-background text-foreground transition ease-out hover:bg-secondary",
                            disabled: *loading.read(),
                            onclick: on_microsoft,
                            svg {
                                width: "20",
                                height: "20",
                                view_box: "0 0 21 21",
                                fill: "none",
                                xmlns: "http://www.w3.org/2000/svg",
                                rect { x: "1", y: "1", width: "9", height: "9", fill: "#F25022" }
                                rect { x: "11", y: "1", width: "9", height: "9", fill: "#7FBA00" }
                                rect { x: "1", y: "11", width: "9", height: "9", fill: "#00A4EF" }
                                rect { x: "11", y: "11", width: "9", height: "9", fill: "#FFB900" }
                            }
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
