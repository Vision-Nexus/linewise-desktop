use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

mod bundle_ffmpeg;
mod cmd;
mod generate_icons;

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "Build and packaging tasks for linewise-desktop"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bundle FFmpeg shared libraries (and transitive deps on macOS/Linux)
    /// into the cargo-bundle output for the given target. Run after
    /// `cargo bundle --release --target <triple>`.
    BundleFfmpeg {
        #[arg(long)]
        target: String,
        /// Build a DMG after bundling (macOS only).
        #[arg(long, default_value_t = false)]
        create_dmg: bool,
    },
    /// Regenerate platform icons (.png/.ico/.icns) from assets/icons/logo.svg.
    GenerateIcons,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = workspace_root();
    match cli.cmd {
        Cmd::BundleFfmpeg { target, create_dmg } => bundle_ffmpeg::run(&root, &target, create_dmg),
        Cmd::GenerateIcons => generate_icons::run(&root),
    }
}

/// Resolve the workspace root by walking up from CARGO_MANIFEST_DIR
/// (which points at crates/xtask/) two levels.
fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("xtask manifest sits two levels below the workspace root")
        .to_path_buf()
}
