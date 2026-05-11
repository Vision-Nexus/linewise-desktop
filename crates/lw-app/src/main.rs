mod app;
mod components;
mod hooks;
pub mod icons;
mod state;
pub mod styles;

use std::borrow::Cow;

use dioxus::desktop::{Config, WindowCloseBehaviour};
use dioxus::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static LOGIN_IMG: &[u8] = include_bytes!("../../../assets/login-img.png");

const SENTRY_DSN: &str = "https://cf5b74f304f4ea35de113d3ac566b957@o4509472431407104.ingest.us.sentry.io/4511116827820032";

fn main() {
    // Initialize Sentry — must be before tracing so the guard lives longest
    let environment = lw_core::config::AppConfig::load()
        .map(|c| match c.server.environment {
            lw_core::config::Environment::Dev => "dev",
            lw_core::config::Environment::Testing => "testing",
            lw_core::config::Environment::Production => "production",
        })
        .unwrap_or("dev");

    let _sentry_guard = sentry::init((
        SENTRY_DSN,
        sentry::ClientOptions {
            release: sentry::release_name!(),
            environment: Some(std::borrow::Cow::from(environment)),
            traces_sample_rate: 0.2,
            ..Default::default()
        },
    ));

    // Tracing: fmt layer for console + sentry layer for error reporting
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .with(sentry_tracing::layer())
        .init();

    tracing::info!("Starting Linewise Upload");

    // Initialize FFmpeg library (bundled path → system fallback)
    if let Err(e) = lw_core::transcode::init() {
        tracing::warn!("FFmpeg not available — transcoding disabled: {e}");
    }

    let cfg = Config::new()
        .with_close_behaviour(WindowCloseBehaviour::WindowHides)
        .with_menu(None)
        .with_custom_protocol("localasset", |_webview_id, request| {
            let uri = request.uri().to_string();
            let path = request.uri().path();
            tracing::debug!("localasset request: uri={uri} path={path}");
            let (body, content_type): (Cow<'static, [u8]>, &str) =
                match path.trim_start_matches('/') {
                    "login-img.png" => (Cow::Borrowed(LOGIN_IMG), "image/png"),
                    _ => (Cow::Borrowed(b"Not Found" as &[u8]), "text/plain"),
                };
            dioxus::desktop::wry::http::Response::builder()
                .header("Content-Type", content_type)
                .header("Access-Control-Allow-Origin", "*")
                .body(body)
                .expect("failed to build protocol response")
        })
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title("Linewise Upload")
                .with_inner_size(dioxus::desktop::LogicalSize::new(1200.0, 750.0)),
        );

    LaunchBuilder::desktop().with_cfg(cfg).launch(app::App);
}
