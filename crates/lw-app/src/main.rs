mod app;
mod components;
mod hooks;
mod state;
pub mod styles;

use dioxus::desktop::{Config, WindowCloseBehaviour};
use dioxus::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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

    tracing::info!("Starting Linewise Desktop");

    // Initialize FFmpeg library (bundled path → system fallback)
    if let Err(e) = lw_core::transcode::init() {
        tracing::warn!("FFmpeg not available — transcoding disabled: {e}");
    }

    let cfg = Config::new()
        .with_close_behaviour(WindowCloseBehaviour::WindowHides)
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title("Linewise Desktop")
                .with_inner_size(dioxus::desktop::LogicalSize::new(900.0, 640.0)),
        );

    LaunchBuilder::desktop().with_cfg(cfg).launch(app::App);
}
