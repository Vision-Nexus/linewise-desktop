use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::bundle_ffmpeg::{copy_dir_all, copy_license_bundle, exiftool_dist};
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
    for cli in ["ffmpeg.exe", "ffprobe.exe"] {
        let src = bin_dir.join(cli);
        if src.is_file() {
            std::fs::copy(&src, release_dir.join(cli))?;
            eprintln!("  Copied {cli}");
        } else {
            eprintln!("  Warning: {cli} not found at {}", src.display());
        }
    }

    ensure_dir(&release_dir)?;
    copy_dlls(&bin_dir, &release_dir, REQUIRED_DLL_PREFIXES, true)?;
    copy_dlls(&bin_dir, &release_dir, OPTIONAL_DLL_PREFIXES, false)?;

    // ExifTool — the Windows STANDALONE build: `exiftool.exe` (the launcher,
    // renamed from `exiftool(-k).exe`) PLUS its `exiftool_files/` folder, which
    // holds the PAR-packed Perl interpreter + modules. Both must sit next to each
    // other (no system Perl needed). EXIFTOOL_DIST is the directory holding them.
    let exiftool_dist = exiftool_dist()?;
    let exiftool_exe = exiftool_dist.join("exiftool.exe");
    if !exiftool_exe.is_file() {
        bail!(
            "exiftool.exe not found under EXIFTOOL_DIST ({}) — Windows needs the \
             standalone build (rename exiftool(-k).exe → exiftool.exe)",
            exiftool_dist.display()
        );
    }
    std::fs::copy(&exiftool_exe, release_dir.join("exiftool.exe"))
        .with_context(|| format!("copy {}", exiftool_exe.display()))?;
    eprintln!("  Copied exiftool.exe");

    // The PAR cache folder ships next to the launcher in modern standalone
    // builds. Older single-file builds don't have it — only copy when present.
    let files_src = exiftool_dist.join("exiftool_files");
    if files_src.is_dir() {
        copy_dir_all(&files_src, &release_dir.join("exiftool_files"))?;
        eprintln!("  Copied exiftool_files/");
    } else {
        eprintln!("  No exiftool_files/ (single-file standalone) — skipping");
    }

    eprintln!(
        "Done bundling FFmpeg + ExifTool into {}",
        release_dir.display()
    );
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
