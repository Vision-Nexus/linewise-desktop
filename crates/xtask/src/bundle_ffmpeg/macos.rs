use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::bundle_ffmpeg::{
    OPTIONAL_LIBS, REQUIRED_LIBS, SharedLibKind, copy_license_bundle, find_major_versioned,
};
use crate::cmd::{capture, ensure_dir, run};

pub fn bundle(root: &Path, target: &str, create_dmg: bool) -> Result<()> {
    let app_bundle = root
        .join("target")
        .join(target)
        .join("release/bundle/osx/Linewise Desktop.app");
    if !app_bundle.is_dir() {
        bail!("App bundle not found at {}", app_bundle.display());
    }

    let frameworks = app_bundle.join("Contents/Frameworks");
    let resources = app_bundle.join("Contents/Resources");
    let main_bin = app_bundle.join("Contents/MacOS/linewise-desktop");
    ensure_dir(&frameworks)?;
    ensure_dir(&resources)?;

    copy_license_bundle(root, &resources.join("licenses"))?;

    let ffmpeg_prefix = ffmpeg_prefix()?;
    eprintln!("Using FFmpeg from: {}", ffmpeg_prefix.display());
    let ffmpeg_lib_dir = ffmpeg_prefix.join("lib");

    // Stage 1 — copy ffmpeg CLI into Resources/. We rewrite its load
    // commands later, after all transitive deps land in Frameworks/.
    let ffmpeg_cli_src = ffmpeg_prefix.join("bin/ffmpeg");
    let ffmpeg_cli_dst = resources.join("ffmpeg");
    std::fs::copy(&ffmpeg_cli_src, &ffmpeg_cli_dst)
        .with_context(|| format!("copy ffmpeg CLI: {}", ffmpeg_cli_src.display()))?;
    set_executable(&ffmpeg_cli_dst)?;
    eprintln!("  Copied ffmpeg binary → Resources/");

    // Stage 2 — bundle the av* libraries plus every transitive dep dyld
    // would resolve from /opt/homebrew on the build host. The recursive
    // walk is the core of the macOS-specific story; bash couldn't track
    // visited dylibs across recursion levels.
    let mut bundled: HashSet<String> = HashSet::new();
    for stem in REQUIRED_LIBS {
        let path = find_major_versioned(&ffmpeg_lib_dir, stem, SharedLibKind::Dylib)?
            .with_context(|| {
                format!(
                    "required FFmpeg library {stem}.*.dylib not under {}",
                    ffmpeg_lib_dir.display()
                )
            })?;
        bundle_dylib_recursive(&path, &frameworks, &mut bundled)?;
    }
    for stem in OPTIONAL_LIBS {
        if let Some(path) = find_major_versioned(&ffmpeg_lib_dir, stem, SharedLibKind::Dylib)? {
            bundle_dylib_recursive(&path, &frameworks, &mut bundled)?;
        } else {
            eprintln!("  Skipped {stem} (not in this FFmpeg build)");
        }
    }

    // Stage 3 — rewrite the app binary and ffmpeg CLI so every non-system
    // LC_LOAD_DYLIB points at @rpath/<basename>, then add the rpath that
    // resolves to Contents/Frameworks/.
    rewrite_consumer(&main_bin, &bundled)?;
    rewrite_consumer(&ffmpeg_cli_dst, &bundled)?;
    add_rpath_if_missing(&main_bin, "@executable_path/../Frameworks")?;
    add_rpath_if_missing(&ffmpeg_cli_dst, "@executable_path/../Frameworks")?;

    eprintln!("Done bundling FFmpeg into {}", app_bundle.display());

    // Stage 4 — codesign. install_name_tool invalidates each Mach-O's
    // signature, so signing has to come last. Hardened runtime is left
    // off intentionally: it enforces library validation, which a
    // self-signed identity (no Team ID) can't satisfy for our bundled
    // dylibs. Hardened runtime is only needed for Apple notarization.
    let identity = std::env::var("MACOS_SIGNING_IDENTITY").unwrap_or_else(|_| "-".into());
    eprintln!("Codesigning {} as: {identity}", app_bundle.display());
    run(
        "codesign",
        [
            "--force".as_ref(),
            "--deep".as_ref(),
            "--timestamp=none".as_ref(),
            "--sign".as_ref(),
            identity.as_str().as_ref(),
            app_bundle.as_os_str(),
        ],
    )?;
    run(
        "codesign",
        [
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            app_bundle.as_os_str(),
        ],
    )?;

    if create_dmg {
        let dmg_path = root
            .join("target")
            .join(format!("linewise-desktop-macos-{target}.dmg"));
        run(
            "hdiutil",
            [
                "create".as_ref(),
                "-volname".as_ref(),
                "Linewise Desktop".as_ref(),
                "-srcfolder".as_ref(),
                app_bundle.as_os_str(),
                "-ov".as_ref(),
                "-format".as_ref(),
                "UDZO".as_ref(),
                dmg_path.as_os_str(),
            ],
        )?;
        eprintln!("Created DMG: {}", dmg_path.display());
    }

    Ok(())
}

/// Resolve the FFmpeg install prefix. Honours FFMPEG_DIR (set in CI),
/// falls back to `brew --prefix ffmpeg`. We don't try to guess from
/// /opt/homebrew/lib — that's a different package layout (ffmpeg-full
/// installed keg-only) and the bin/lib split below assumes Homebrew's
/// canonical opt/ symlink.
fn ffmpeg_prefix() -> Result<PathBuf> {
    if let Ok(env) = std::env::var("FFMPEG_DIR") {
        return Ok(PathBuf::from(env));
    }
    let out = capture("brew", ["--prefix", "ffmpeg"])
        .context("FFMPEG_DIR not set and `brew --prefix ffmpeg` failed")?;
    Ok(PathBuf::from(out.trim()))
}

/// Copy `src` into `frameworks/`, rewrite its install name, then walk
/// every non-system load command — for each, recurse, then rewrite the
/// reference inside the just-copied dylib to @rpath/<basename>. This is
/// what lets dyld resolve `libvpx.12.dylib` and the other transitive
/// Homebrew deps on a machine without Homebrew.
fn bundle_dylib_recursive(
    src: &Path,
    frameworks: &Path,
    bundled: &mut HashSet<String>,
) -> Result<()> {
    let basename = src
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("non-UTF8 dylib path: {}", src.display()))?
        .to_owned();
    if !bundled.insert(basename.clone()) {
        return Ok(());
    }

    let dst = frameworks.join(&basename);
    std::fs::copy(src, &dst).with_context(|| format!("copy {} → {}", src.display(), dst.display()))?;
    make_writable(&dst)?;
    eprintln!("  Copied {basename} → Frameworks/");

    run(
        "install_name_tool",
        [
            "-id".as_ref(),
            format!("@rpath/{basename}").as_str().as_ref(),
            dst.as_os_str(),
        ],
    )?;

    for dep in load_commands(&dst)? {
        if !is_external_dep(&dep) {
            continue;
        }
        let dep_path = PathBuf::from(&dep);
        let dep_basename = dep_path
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("dylib load path has no filename: {dep}"))?
            .to_owned();

        // Recurse before rewriting, so the dependency lands in
        // Frameworks/ first. Order doesn't change correctness — the
        // rewrite below targets the parent dylib, not the child — but
        // it keeps log output bottom-up and makes failures easier to
        // attribute.
        bundle_dylib_recursive(&dep_path, frameworks, bundled)?;

        run(
            "install_name_tool",
            [
                "-change".as_ref(),
                dep.as_str().as_ref(),
                format!("@rpath/{dep_basename}").as_str().as_ref(),
                dst.as_os_str(),
            ],
        )?;
    }

    Ok(())
}

/// Rewrite every external load command in `consumer` (the main app
/// binary or the ffmpeg CLI) to @rpath/<basename>, but only when a
/// dylib by that basename has been bundled. Unbundled deps are left
/// alone — they're system frameworks dyld will resolve normally.
fn rewrite_consumer(consumer: &Path, bundled: &HashSet<String>) -> Result<()> {
    for dep in load_commands(consumer)? {
        if !is_external_dep(&dep) {
            continue;
        }
        let dep_basename = Path::new(&dep)
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned);
        let Some(name) = dep_basename else { continue };
        if !bundled.contains(&name) {
            continue;
        }
        run(
            "install_name_tool",
            [
                "-change".as_ref(),
                dep.as_str().as_ref(),
                format!("@rpath/{name}").as_str().as_ref(),
                consumer.as_os_str(),
            ],
        )?;
    }
    Ok(())
}

/// True for absolute paths into Homebrew (or anywhere else outside the
/// system roots), where dyld would otherwise try to load the dylib from
/// the build host's filesystem at runtime.
fn is_external_dep(load_path: &str) -> bool {
    !(load_path.starts_with("/usr/lib/")
        || load_path.starts_with("/System/")
        || load_path.starts_with("@rpath/")
        || load_path.starts_with("@loader_path/")
        || load_path.starts_with("@executable_path/"))
}

/// Read `otool -L <path>` and return the load-command paths, dropping
/// the header line and the dylib's own LC_ID self-reference. After we
/// rewrite the install name to @rpath/<basename>, otool prints that
/// line first; before rewrite, it's a path ending in /<basename>.
fn load_commands(path: &Path) -> Result<Vec<String>> {
    let out = capture("otool", ["-L".as_ref(), path.as_os_str()])?;
    let self_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_default();
    Ok(out
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .filter(|p| !is_self_id(p, &self_name))
        .collect())
}

fn is_self_id(load: &str, self_name: &str) -> bool {
    if self_name.is_empty() {
        return false;
    }
    load == self_name
        || load == format!("@rpath/{self_name}")
        || load.ends_with(&format!("/{self_name}"))
}

fn add_rpath_if_missing(target: &Path, rpath: &str) -> Result<()> {
    // install_name_tool errors if the rpath is already present; ignore
    // a non-zero exit here. There's no clean way to query existing
    // rpaths short of parsing `otool -l`, and a duplicate-add failure
    // is harmless.
    let _ = std::process::Command::new("install_name_tool")
        .args([
            "-add_rpath".as_ref(),
            rpath.as_ref(),
            target.as_os_str(),
        ])
        .status();
    Ok(())
}

fn make_writable(path: &Path) -> Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perms.mode() | 0o200;
        perms.set_mode(mode);
    }
    #[cfg(not(unix))]
    {
        perms.set_readonly(false);
    }
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
