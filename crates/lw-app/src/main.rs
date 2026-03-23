mod app;
mod components;
mod hooks;
mod state;

use dioxus::desktop::{Config, WindowCloseBehaviour};
use dioxus::prelude::*;
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Starting Linewise Desktop");

    let cfg = Config::new()
        .with_close_behaviour(WindowCloseBehaviour::WindowHides)
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title("Linewise Desktop")
                .with_inner_size(dioxus::desktop::LogicalSize::new(900.0, 640.0)),
        );

    LaunchBuilder::desktop()
        .with_cfg(cfg)
        .launch(app::App);
}
