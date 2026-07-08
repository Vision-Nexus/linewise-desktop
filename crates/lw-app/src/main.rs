#![windows_subsystem = "windows"]

mod app;
mod components;
mod hooks;
pub mod icons;
mod single_instance;
mod state;
pub mod styles;

use std::sync::LazyLock;

use base64::Engine as _;
use dioxus::desktop::muda::{Menu, PredefinedMenuItem, Submenu};
use dioxus::desktop::{Config, WindowCloseBehaviour};
use dioxus::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static LOGIN_IMG: &[u8] = include_bytes!("../../../assets/login-img.png");

/// The login background as a self-contained `data:` URI, encoded once on first
/// use. Served this way instead of a `localasset://` custom protocol because
/// that scheme doesn't resolve uniformly across platforms (it 404'd the image on
/// Windows WebView2); a `data:` URI needs no protocol registration and renders
/// identically everywhere. `pub` so `components::login` can reference it.
pub static LOGIN_IMG_DATA_URI: LazyLock<String> = LazyLock::new(|| {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(LOGIN_IMG)
    )
});

const SENTRY_DSN: &str = "https://cf5b74f304f4ea35de113d3ac566b957@o4509472431407104.ingest.us.sentry.io/4511116827820032";

fn main() {
    // Load config once — used for both Sentry environment and log level.
    // Falling back to defaults is deliberate: we still want a usable app if
    // config.toml is missing or malformed on a fresh install.
    let config = match lw_core::config::AppConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: failed to load config ({e}); using defaults");
            lw_core::config::AppConfig::default()
        }
    };

    let environment = match config.server.environment {
        lw_core::config::Environment::Dev => "dev",
        lw_core::config::Environment::Testing => "testing",
        lw_core::config::Environment::Production => "production",
    };

    // Initialize Sentry — must be before tracing so the guard lives longest
    let _sentry_guard = sentry::init((
        SENTRY_DSN,
        sentry::ClientOptions {
            release: sentry::release_name!(),
            environment: Some(std::borrow::Cow::from(environment)),
            traces_sample_rate: 0.2,
            // Release Health: emit a session per app run (started here, ended
            // when `_sentry_guard` drops at exit) so Sentry counts ALL desktop
            // users + active-by-version — not just the ones who hit an error.
            // Sessions are attributed to the logged-in user set on the scope in
            // app.rs after whoami. Application mode (the default) = one session
            // per process run, which is what we want for a desktop app.
            auto_session_tracking: true,
            // Dioxus 0.7 / wry logs `error!("Webview {n} was already connected.
            // Rejecting new connection.")` on every webview reconnect. The
            // rejection is a benign framework no-op, not an app fault, but
            // `sentry_tracing` captures `error!` as an Event — making this the
            // single highest-volume desktop "error" in Sentry. We can't lower
            // the framework's emission level (it lives in a dependency), so we
            // drop exactly this line here: `before_send` sees the rendered
            // message and returns `None` to discard it, leaving every other
            // error (including real wry / dioxus faults) untouched. Matched on
            // the stable substring, not the per-window webview index.
            before_send: Some(std::sync::Arc::new(|event| {
                let is_webview_reconnect_noise = event
                    .message
                    .as_deref()
                    .is_some_and(|m| m.contains("already connected"))
                    || event
                        .logentry
                        .as_ref()
                        .is_some_and(|l| l.message.contains("already connected"));
                if is_webview_reconnect_noise {
                    None
                } else {
                    Some(event)
                }
            })),
            ..Default::default()
        },
    ));

    // Rolling daily file appender, retaining the last 14 days. The
    // WorkerGuard returned by `non_blocking` must live for the whole
    // program — drop it and the background flush thread shuts down,
    // losing buffered lines. We deliberately abort startup if file
    // logging cannot be set up: this is an internal-user app where
    // logs are a debugging requirement, not a nice-to-have.
    let log_dir = lw_core::logging::ensure_log_dir().expect("failed to create log directory");
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(lw_core::logging::LOG_FILENAME_PREFIX)
        .filename_suffix(lw_core::logging::LOG_FILENAME_SUFFIX)
        .max_log_files(14)
        .build(&log_dir)
        .expect("failed to build rolling log file appender");
    let (file_writer, _file_guard) = tracing_appender::non_blocking(file_appender);

    // Filter precedence: RUST_LOG > config.app.log_filter > built-in default.
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&config.app.log_filter))
        .unwrap_or_else(|_| EnvFilter::new(lw_core::logging::DEFAULT_LOG_FILTER));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .with(sentry_tracing::layer())
        .init();

    tracing::info!("Starting Linewise Upload");

    // Single-instance guard. The data dir is build-independent, so a debug
    // build and a downloaded release share the same config.toml / linewise.db /
    // keyring — running two at once corrupts the SQLite queue and races config
    // writes. A second launch hands "show the window" off to the running
    // instance (raised via the same path as the tray "show" item) and exits
    // without opening a window. `LINEWISE_DESKTOP_ALLOW_MULTIPLE` skips the
    // guard for development. Done after logging so the handoff is observable.
    match single_instance::acquire() {
        single_instance::GuardOutcome::Continue => {}
        single_instance::GuardOutcome::AlreadyRunning => {
            tracing::info!("Another instance is already running; exiting after raising it");
            return;
        }
    }

    // Initialize FFmpeg library (bundled path → system fallback)
    if let Err(e) = lw_core::transcode::init() {
        tracing::warn!("FFmpeg not available — transcoding disabled: {e}");
    }

    // The login background is embedded and served as a `data:` URI (see
    // LOGIN_IMG_DATA_URI) rather than a custom protocol, so no `localasset`
    // handler is registered here.
    let cfg = Config::new()
        .with_close_behaviour(WindowCloseBehaviour::WindowHides)
        .with_menu(Some(build_app_menu()))
        .with_window(build_window());

    LaunchBuilder::desktop().with_cfg(cfg).launch(app::App);
}

/// Builds the main window with the right chrome for the current OS.
///
/// macOS uses the native transparent-titlebar + fullsize-content-view
/// pattern: the traffic-light buttons stay (users expect them in their
/// usual position) and the window title is hidden, while our custom bar
/// renders underneath and extends to the top edge. Content is padded to
/// clear the traffic lights via `TitleBar`.
///
/// Windows and Linux go fully frameless — we render our own min/max/close
/// buttons in `WindowControls`.
#[cfg(target_os = "macos")]
fn build_window() -> dioxus::desktop::WindowBuilder {
    use dioxus::desktop::tao::platform::macos::WindowBuilderExtMacOS;
    dioxus::desktop::WindowBuilder::new()
        .with_title("Linewise Upload")
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true)
        .with_resizable(true)
        .with_inner_size(dioxus::desktop::LogicalSize::new(1200.0, 750.0))
}

#[cfg(not(target_os = "macos"))]
fn build_window() -> dioxus::desktop::WindowBuilder {
    dioxus::desktop::WindowBuilder::new()
        .with_title("Linewise Upload")
        .with_decorations(false)
        .with_resizable(true)
        .with_inner_size(dioxus::desktop::LogicalSize::new(1200.0, 750.0))
}

// Builds the app menu so the OS registers clipboard accelerators
// (Ctrl/Cmd + C/V/X and Undo/Redo) for the WebView. Without this,
// `Config::with_decorations(false)` on Windows causes dioxus-desktop
// to auto-force the menu to None (see dioxus-desktop config.rs), which
// strips the muda accelerator table and leaves text inputs unable to
// copy or paste. The submenu is invisible on Windows (no native frame
// hosts a menu bar) but its accelerators still bind via
// `TranslateAcceleratorW`. On macOS it appears as a standard Edit menu.
//
// `select_all` is intentionally omitted: muda's predefined item routes
// Ctrl+A to "select all DOM content" (highlighting buttons, sidebars,
// the whole window) regardless of focus, which is jarring. Without a
// bound accelerator the WebView handles Ctrl+A natively — inside an
// input it selects the input's text, elsewhere it does nothing.
fn build_app_menu() -> Menu {
    let menu = Menu::new();
    let edit = Submenu::new("Edit", true);
    edit.append_items(&[
        &PredefinedMenuItem::undo(None),
        &PredefinedMenuItem::redo(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(None),
        &PredefinedMenuItem::copy(None),
        &PredefinedMenuItem::paste(None),
    ])
    .expect("failed to build edit submenu");
    menu.append(&edit).expect("failed to append edit submenu");
    menu
}
