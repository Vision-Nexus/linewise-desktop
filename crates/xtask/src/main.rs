use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

mod bundle_ffmpeg;
mod cmd;
mod generate_icons;
mod release;

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
    /// Bundle the external tools into the cargo-bundle output for the given
    /// target: FFmpeg shared libs (+ transitive deps on macOS/Linux), the
    /// ffmpeg/ffprobe CLIs, and ExifTool (capture-metadata embed/read-back).
    /// Requires EXIFTOOL_DIST. Run after `cargo bundle --release --target <triple>`.
    BundleFfmpeg {
        #[arg(long)]
        target: String,
        /// Build a DMG after bundling (macOS only).
        #[arg(long, default_value_t = false)]
        create_dmg: bool,
    },
    /// Regenerate platform icons (.png/.ico/.icns) from assets/icons/logo.svg.
    GenerateIcons,
    /// Cut a release: rewrite workspace version, refresh Cargo.lock,
    /// commit, and tag. Push is opt-in via `--push`. The tag must
    /// start with `v`, e.g. `v0.0.10-test`.
    Release {
        /// Tag to create (e.g. `v0.0.10-test`).
        tag: String,
        /// Skip the clean-tree precheck. Use only for emergency releases.
        #[arg(long, default_value_t = false)]
        allow_dirty: bool,
        /// Push the new commit and tag to `origin` after creating them.
        #[arg(long, default_value_t = false)]
        push: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = workspace_root();
    match cli.cmd {
        Cmd::BundleFfmpeg { target, create_dmg } => bundle_ffmpeg::run(&root, &target, create_dmg),
        Cmd::GenerateIcons => generate_icons::run(&root),
        Cmd::Release {
            tag,
            allow_dirty,
            push,
        } => release::run(&root, &tag, allow_dirty, push),
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
