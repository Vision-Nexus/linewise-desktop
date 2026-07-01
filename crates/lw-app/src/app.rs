use crate::components::login::LoginPage;
use crate::components::transfer_panel::TransferPanel;
use crate::components::version_banner::{VersionBlockedScreen, VersionUpdateBanner};
use crate::state::{AppState, CoreServices};
use dioxus::desktop::WindowCloseBehaviour;
use dioxus::desktop::trayicon::{init_tray_icon, menu::*};
use dioxus::prelude::*;
use lw_core::version_check::{self, VersionStatus};
use std::sync::Arc;

const TAILWIND_CSS: &str = include_str!("../tailwind.generated.css");
const DX_COMPONENTS_THEME_CSS: &str = include_str!("../assets/dx-components-theme.css");

// Per-component stylesheets are inlined here (same mechanism as TAILWIND_CSS
// above) because Dioxus's `asset!()` / `#[css_module]` asset resolver doesn't
// reliably deliver files to WebView2 on Windows when built with plain `cargo`.
const SWITCH_CSS: &str = include_str!("components/switch/style.css");
const SLIDER_CSS: &str = include_str!("components/slider/style.css");
const TOGGLE_GROUP_CSS: &str = include_str!("components/toggle_group/style.css");
const SHEET_CSS: &str = include_str!("components/sheet/style.css");
const PROGRESS_CSS: &str = include_str!("components/progress/style.css");

/// Global CSS for hover/active states (can't do `:hover` in inline styles).
/// Lives in `global.css` next to this file so editor tooling treats it as
/// CSS, not as a Rust string literal.
const GLOBAL_CSS: &str = include_str!("global.css");

/// Boot phase. `App` drives `CoreServices::init()` off-thread and flips
/// through these states. `Ready` is the only branch that provides the
/// `CoreServices` context and mounts `AppInner`, so downstream components
/// (which unconditionally `use_context::<CoreServices>()`) never see a
/// missing or half-constructed service.
#[derive(Clone)]
enum BootState {
    Initializing,
    Ready(Arc<CoreServices>),
    Failed(String),
}

#[component]
pub fn App() -> Element {
    // AppState is cheap and never fails, so it's safe to provide here and
    // let the recovery screen read from it too.
    use_context_provider(AppState::new);

    // Tray init has to happen exactly once over the lifetime of the
    // process; do it here so it's live even while we're still booting or
    // recovering from a DB error.
    use_hook(|| {
        let menu = build_tray_menu();
        init_tray_icon(menu, None);
    });
    // Tray menu item clicks (and app-menu clicks) arrive on muda's single
    // global MenuEvent stream. dioxus forwards that stream as `MudaMenuEvent`
    // because its menubar receiver wins muda's `OnceCell` handler slot — the
    // later `set_tray_icon_receiver` call that would route to `TrayMenuEvent`
    // is silently dropped (OnceCell::set is a no-op once set). So
    // `use_tray_menu_event_handler` never fires when an app menu is also
    // configured; `use_muda_event_handler` is the stream that actually
    // carries our tray "show"/"quit" items.
    dioxus::desktop::use_muda_event_handler(move |event| {
        match event.id().0.as_str() {
            "show" => {
                let window = dioxus::desktop::window();
                window.set_visible(true);
                window.set_focus();
            }
            "quit" => {
                // Don't `std::process::exit` from this UI-thread callback: on
                // Windows that runs WebView2's STA-COM teardown while the
                // message pump is stopped and can deadlock. Switch to a real
                // close and drop the last window so `exit_on_last_window_close`
                // sets `ControlFlow::Exit` and the loop unwinds cleanly.
                let window = dioxus::desktop::window();
                window.set_close_behavior(WindowCloseBehaviour::WindowCloses);
                window.close();
            }
            _ => {}
        }
    });
    dioxus::desktop::use_tray_icon_event_handler(move |event| {
        use dioxus::desktop::trayicon::{MouseButton, MouseButtonState, TrayIconEvent};
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let window = dioxus::desktop::window();
            window.set_visible(true);
            window.set_focus();
        }
    });

    // Single-instance "show" relay. When a second launch is attempted, the
    // primary instance's background accept loop notifies here; we raise the
    // window using the exact same calls as the tray "show" item above so the
    // behavior is identical. Runs for the app's lifetime, one wake per signal.
    use_future(|| async {
        loop {
            crate::single_instance::show_requests().notified().await;
            let window = dioxus::desktop::window();
            window.set_visible(true);
            window.set_focus();
        }
    });

    let mut boot = use_signal(|| BootState::Initializing);

    // Startup version check. Runs once at mount, in parallel with
    // CoreServices::init(). Failure is non-blocking — a flaky GitHub
    // shouldn't gate a working app, so any error is logged at warn! and
    // version_status stays None (treated as "unknown" by the renderer).
    let mut app_state_for_version = use_context::<AppState>();
    use_future(move || async move {
        match version_check::check_version(env!("CARGO_PKG_VERSION")).await {
            Ok(status) => app_state_for_version.version_status.set(Some(status)),
            Err(e) => tracing::warn!("Version check failed: {e}"),
        }
    });

    // Defensive on-screen placement. On some Windows display configurations the
    // freshly-created window lands far off-screen (observed at -25600,-25600),
    // so only the tray icon is visible and the app looks like it never opened.
    // After mount, if the window is parked off-screen, recentre it on the
    // primary monitor and show+focus it. A normal placement (x/y within sane
    // bounds) is left untouched. The framework positions the window once at
    // creation and doesn't move it afterwards, so doing this post-mount sticks.
    use_future(|| async move {
        use dioxus::desktop::tao::dpi::PhysicalPosition;
        let window = dioxus::desktop::window();
        // Centre on the primary monitor. The framework's initial placement is
        // unreliable on some Windows display setups — it sometimes parks the
        // window far off-screen (observed at -25600,-25600), leaving only the
        // tray icon visible. Centring unconditionally after mount keeps the
        // window reliably on-screen; the post-mount position set is not
        // overridden by the framework afterwards.
        if let Some(monitor) = window.primary_monitor() {
            let mpos = monitor.position();
            let msize = monitor.size();
            let wsize = window.outer_size();
            let x = mpos.x + ((msize.width as i32 - wsize.width as i32) / 2).max(0);
            let y = mpos.y + ((msize.height as i32 - wsize.height as i32) / 2).max(0);
            window.set_outer_position(PhysicalPosition::new(x, y));
            tracing::info!("centred window on primary monitor at ({x}, {y})");
        }
        window.set_visible(true);
        window.set_focus();
    });

    // Restart trigger: any leaf component (e.g. the environment switcher
    // in settings) can call `AppState::request_restart()`, which bumps
    // `restart_token`. We watch the token and flip `boot` back to
    // `Initializing` whenever it changes — but only when we're not
    // already initializing, so the bootstrap effect below sees a real
    // edge transition.
    let app_state_restart = use_context::<AppState>();
    use_effect(move || {
        let token = *app_state_restart.restart_token.read();
        if token > 0 && !matches!(&*boot.peek(), BootState::Initializing) {
            boot.set(BootState::Initializing);
        }
    });

    // Bootstrap effect — runs CoreServices::init() every time `boot`
    // settles on `Initializing`. We can't use `use_future` here because
    // it fires exactly once at mount; `use_effect` re-fires on every
    // signal change it reads, which is what we need for retry and
    // environment switching to actually re-init.
    use_effect(move || {
        if !matches!(&*boot.read(), BootState::Initializing) {
            return;
        }
        spawn(async move {
            match CoreServices::init().await {
                Ok(services) => {
                    tracing::info!("boot complete");
                    boot.set(BootState::Ready(Arc::new(services)));
                }
                Err(e) => {
                    tracing::warn!("Core services failed to initialize: {e}");
                    boot.set(BootState::Failed(e));
                }
            }
        });
    });

    let state = boot.read().clone();

    // The version-check status is read here, separately from BootState,
    // so that an `Unsupported` answer gates rendering regardless of how
    // boot is going. CoreServices and the version check are intentionally
    // independent — a transient block from a GitHub blip shouldn't feel
    // sticky after a retry.
    let app_state_for_block = use_context::<AppState>();
    let is_blocked = matches!(
        &*app_state_for_block.version_status.read(),
        Some(VersionStatus::Unsupported { .. })
    );

    rsx! {
        style { "{GLOBAL_CSS}" }
        style { "{TAILWIND_CSS}" }
        style { "{DX_COMPONENTS_THEME_CSS}" }
        style { "{SWITCH_CSS}" }
        style { "{SLIDER_CSS}" }
        style { "{TOGGLE_GROUP_CSS}" }
        style { "{SHEET_CSS}" }
        style { "{PROGRESS_CSS}" }
        div {
            class: "flex flex-col h-screen w-screen overflow-hidden",
            crate::components::title_bar::TitleBar {}
            VersionUpdateBanner {}
            crate::components::toast::ToastOverlay {}
            div {
                class: "flex-1 min-h-0 overflow-hidden",
                if is_blocked {
                    VersionBlockedScreen {}
                } else {
                    match state {
                        BootState::Initializing => rsx! {
                            div { class: "loading-screen",
                                span { class: "spinner" }
                                span { "Starting up..." }
                            }
                        },
                        BootState::Failed(error) => rsx! {
                            DbErrorScreen {
                                error,
                                on_retry: move |_| boot.set(BootState::Initializing),
                            }
                        },
                        BootState::Ready(services) => rsx! {
                            AuthedShell { services }
                        },
                    }
                }
            }

            // Global repair modal — mounted at the shell level (not under
            // AuthedShell/MainView) so the trigger from the title bar
            // works during Initializing, Failed, login, and authed
            // states. Repair is recovery-only; it must reach a wedged
            // app even before sign-in completes.
            if *app_state_for_block.show_repair.read() {
                crate::components::repair_modal::RepairModal {
                    on_close: {
                        let mut app_state = app_state_for_block.clone();
                        move |_| app_state.show_repair.set(false)
                    },
                }
            }
        }
    }
}

/// Renders the login/main split. Receives the initialized `CoreServices`
/// as a prop and publishes it into the context tree so deeply-nested
/// children (upload queue, chat panel, etc.) can reach it via
/// `use_context`.
#[component]
fn AuthedShell(services: Arc<CoreServices>) -> Element {
    let services_for_ctx: CoreServices = (*services).clone();
    use_context_provider(|| services_for_ctx);

    let mut app_state = use_context::<AppState>();
    app_state.services.set(Some((*services).clone()));
    app_state.config.set(services.config.clone());
    let services = use_context::<CoreServices>();
    let mut restoring = use_signal(|| true);

    let app_state_restore = app_state.clone();
    use_future(move || {
        let auth = services.auth.clone();
        let api = services.api.clone();
        let mut app_state = app_state_restore.clone();
        async move {
            if let Ok(_tokens) = auth.try_restore_session().await {
                tracing::info!("Session restored");
                if let Ok(token) = auth.get_id_token().await {
                    app_state.auth_token.set(token);
                }
                fetch_user_info(&auth, &api, &mut app_state).await;
            }
            restoring.set(false);
        }
    });

    let is_authenticated = *app_state.is_authenticated.read();
    let is_restoring = *restoring.read();

    rsx! {
        // Resident upload runtime: the single event-pump consumer + one-shot
        // startup recovery. Mounted here (above the login/main split) so it
        // never unmounts on navigation and binds to THIS CoreServices'
        // event channel. Renders nothing. See upload_runtime.rs.
        crate::components::upload_runtime::UploadRuntime {}
        if is_restoring {
            div { class: "loading-screen",
                span { class: "spinner" }
                span { "Signing in..." }
            }
        } else if !is_authenticated {
            LoginPage {}
        } else {
            MainView {}
        }
    }
}

#[component]
fn DbErrorScreen(error: String, on_retry: EventHandler<()>) -> Element {
    let mut resetting = use_signal(|| false);
    let mut reset_error: Signal<Option<String>> = use_signal(|| None);
    let mut confirming = use_signal(|| false);

    let on_confirm_reset = move |_| {
        if *resetting.read() {
            return;
        }
        resetting.set(true);
        reset_error.set(None);
        // `reset_local_files` is synchronous file I/O — fast enough to run
        // on the UI thread. On success, flip back to Initializing so the
        // outer bootstrap re-runs.
        match lw_core::db::Database::reset_local_files() {
            Ok(()) => {
                tracing::info!("Local database reset; retrying initialization");
                resetting.set(false);
                confirming.set(false);
                on_retry.call(());
            }
            Err(e) => {
                tracing::error!("Failed to reset local database: {e}");
                reset_error.set(Some(e.to_string()));
                resetting.set(false);
            }
        }
    };

    let db_path = lw_core::config::AppConfig::db_path();
    let db_path_display = db_path.display().to_string();
    let is_busy = *resetting.read();

    rsx! {
        div {
            class: "h-full w-full flex items-center justify-center p-8",
            div {
                class: "max-w-[520px] w-full flex flex-col gap-4 bg-background border border-border rounded-lg p-6 shadow-md",

                h2 {
                    class: "text-lg font-semibold text-foreground",
                    "Couldn't open the local database"
                }
                p {
                    class: "text-sm text-muted-foreground",
                    "Linewise Desktop tracks upload history in a local SQLite file. The app couldn't open or migrate it, so it can't start."
                }
                div {
                    class: "text-xs font-mono bg-destructive-light text-destructive border border-destructive rounded px-3 py-2 whitespace-pre-wrap break-words",
                    "{error}"
                }
                p {
                    class: "text-xs text-muted-foreground",
                    "Database file: "
                    span { class: "font-mono", "{db_path_display}" }
                }

                if *confirming.read() {
                    div {
                        class: "flex flex-col gap-3 border-t border-border pt-4",
                        p {
                            class: "text-sm text-foreground",
                            "Reset deletes the local database. Upload history is lost, but no uploaded files are affected — every completed upload lives on the server."
                        }
                        if let Some(err) = reset_error.read().as_ref() {
                            div {
                                class: "text-xs text-destructive bg-destructive-light border border-destructive rounded px-3 py-2",
                                "{err}"
                            }
                        }
                        div {
                            class: "flex gap-2 justify-end",
                            button {
                                class: "h-9 px-4 text-sm rounded border border-border bg-background text-foreground hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer",
                                disabled: is_busy,
                                onclick: move |_| confirming.set(false),
                                "Cancel"
                            }
                            button {
                                class: "h-9 px-4 text-sm rounded bg-destructive text-destructive-foreground hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer",
                                disabled: is_busy,
                                onclick: on_confirm_reset,
                                if is_busy {
                                    span { class: "spinner spinner-sm mr-1" }
                                    "Resetting..."
                                } else {
                                    "Delete and Retry"
                                }
                            }
                        }
                    }
                } else {
                    div {
                        class: "flex gap-2 justify-end border-t border-border pt-4",
                        button {
                            class: "h-9 px-4 text-sm rounded border border-border bg-background text-foreground hover:bg-accent cursor-pointer",
                            onclick: move |_| on_retry.call(()),
                            "Retry"
                        }
                        button {
                            class: "h-9 px-4 text-sm rounded bg-destructive text-destructive-foreground hover:opacity-90 cursor-pointer",
                            onclick: move |_| confirming.set(true),
                            "Reset local database"
                        }
                    }
                }
            }
        }
    }
}

async fn fetch_user_info(
    auth: &lw_core::auth::AuthService,
    api: &lw_core::api_client::ApiClient,
    app_state: &mut AppState,
) {
    let system_roles = match auth.get_id_token().await {
        Ok(token) => lw_core::auth::claims::decode_unverified(&token).system_roles,
        Err(e) => {
            tracing::warn!("Could not read id_token for claims: {e}");
            Vec::new()
        }
    };
    match api.whoami().await {
        Ok(resp) => {
            if let Some(info) = lw_core::models::UserInfo::from_whoami(resp, system_roles) {
                // Set Sentry user context for error attribution
                sentry::configure_scope(|scope| {
                    scope.set_user(Some(sentry::User {
                        id: Some(info.uid.clone()),
                        email: Some(info.email.clone()),
                        username: info.display_name.clone(),
                        ..Default::default()
                    }));
                });
                app_state.user_info.set(Some(info));
                app_state.is_authenticated.set(true);
            } else {
                tracing::warn!("WhoAmI response has no user");
            }
        }
        Err(e) => {
            tracing::warn!("Failed to fetch user info: {e}");
        }
    }
}

#[component]
fn MainView() -> Element {
    let mut app_state = use_context::<AppState>();

    rsx! {
        div {
            style: "display: flex; height: 100%; position: relative;",

            crate::components::sidebar::Sidebar {}

            // Main content — upload queue
            div {
                style: "flex: 1; display: flex; flex-direction: column; overflow: hidden; min-width: 0; position: relative;",

                // Weak-network prompt: appears when connectivity has been weak
                // for longer than the grace period. Non-modal, above the panel.
                crate::components::weak_network_banner::WeakNetworkBanner {}

                main {
                    style: "flex: 1; overflow-y: auto; padding: 16px;",
                    TransferPanel {}
                }
            }

            // Global settings modal
            if *app_state.show_settings.read() {
                crate::components::settings_modal::SettingsModal {
                    on_close: move |_| app_state.show_settings.set(false),
                }
            }

            // Repair modal lives at the shell level (see app.rs root) so
            // it stays reachable during boot/login.
        }
    }
}

fn build_tray_menu() -> dioxus::desktop::trayicon::DioxusTrayMenu {
    let menu = Menu::new();
    let show = MenuItem::with_id("show", "Open Linewise", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit])
        .expect("failed to build tray menu");
    menu
}
