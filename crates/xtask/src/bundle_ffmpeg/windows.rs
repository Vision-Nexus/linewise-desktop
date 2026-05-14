use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::bundle_ffmpeg::copy_license_bundle;
use crate::cmd::ensure_dir;

/// FFmpeg DLLs we need next to linewise-desktop.exe. The version
/// numbers are part of the filename (e.g. `avcodec-62.dll`) so we glob
/// by prefix rather than picking specific versions.
const REQUIRED_DLL_PREFIXES: &[&str] = &[
    "avcodec-",
    "avformat-",
    "avutil-",
    "swscale-",
    "swresample-",
    "avfilter-",
    "avdevice-",
];

/// postproc is GPL-only and not in every FFmpeg build.
const OPTIONAL_DLL_PREFIXES: &[&str] = &["postproc-"];

pub fn bundle(root: &Path, target: &str) -> Result<()> {
    let release_dir = root.join("target").join(target).join("release");
    let exe = release_dir.join("linewise-desktop.exe");
    if !exe.is_file() {
        bail!("linewise-desktop.exe not found at {}", exe.display());
    }

    copy_license_bundle(root, &release_dir.join("licenses"))?;

    let ffmpeg_dir = std::env::var("FFMPEG_DIR")
        .context("FFMPEG_DIR not set — point it at the extracted FFmpeg shared build")?;
    let ffmpeg_dir = PathBuf::from(ffmpeg_dir);
    eprintln!("Bundling FFmpeg from: {}", ffmpeg_dir.display());

    let bin_dir = ffmpeg_dir.join("bin");
    let ffmpeg_exe = bin_dir.join("ffmpeg.exe");
    if ffmpeg_exe.is_file() {
        std::fs::copy(&ffmpeg_exe, release_dir.join("ffmpeg.exe"))?;
        eprintln!("  Copied ffmpeg.exe");
    } else {
        eprintln!(
            "  Warning: ffmpeg.exe not found at {}",
            ffmpeg_exe.display()
        );
    }

    ensure_dir(&release_dir)?;
    copy_dlls(&bin_dir, &release_dir, REQUIRED_DLL_PREFIXES, true)?;
    copy_dlls(&bin_dir, &release_dir, OPTIONAL_DLL_PREFIXES, false)?;

    eprintln!("Done bundling FFmpeg into {}", release_dir.display());
    Ok(())
}

fn copy_dlls(src_dir: &Path, dst_dir: &Path, prefixes: &[&str], required: bool) -> Result<()> {
    for prefix in prefixes {
        let mut found = false;
        for entry in
            std::fs::read_dir(src_dir).with_context(|| format!("read_dir {}", src_dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.to_ascii_lowercase().ends_with(".dll") {
                continue;
            }
            if !name_str.starts_with(prefix) {
                continue;
            }
            let from = entry.path();
            let to = dst_dir.join(&name);
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} → {}", from.display(), to.display()))?;
            eprintln!("  Copied {name_str}");
            found = true;
        }
        if !found && required {
            bail!("no DLL matched {prefix}* under {}", src_dir.display());
        }
        if !found {
            eprintln!("  Skipped {prefix}* (not in this FFmpeg build)");
        }
    }
    Ok(())
}
