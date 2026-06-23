use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::bundle_ffmpeg::{
    OPTIONAL_LIBS, REQUIRED_LIBS, SharedLibKind, copy_exiftool_unix, copy_license_bundle,
    exiftool_dist, find_major_versioned,
};
use crate::cmd::{capture, ensure_dir, run, which};

/// glibc + low-level runtime components that ship with every Linux
/// distribution. Bundling them risks ABI mismatch with the user's
/// kernel/loader, and shipping a private libc is the kind of thing
/// that breaks Wayland/PulseAudio in subtle ways. Leave them to the
/// user's system loader.
const SYSTEM_DENYLIST: &[&str] = &[
    "libc.so",
    "libm.so",
    "libdl.so",
    "libpthread.so",
    "librt.so",
    "libresolv.so",
    "ld-linux",
    "linux-vdso",
    "libstdc++.so",
    "libgcc_s.so",
    "libgomp.so",
    "libutil.so",
    "libnsl.so",
    "libcrypt.so",
];

pub fn bundle(root: &Path, target: &str) -> Result<()> {
    let bundle_root = root.join("target").join(target).join("release/bundle/deb");

    let deb_file = std::fs::read_dir(&bundle_root)
        .with_context(|| format!("read_dir {}", bundle_root.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "deb"))
        .with_context(|| {
            format!(
                "no .deb under {} — run `cargo bundle --release` first",
                bundle_root.display()
            )
        })?;
    eprintln!("Repacking .deb with bundled FFmpeg: {}", deb_file.display());

    let work_dir = tempdir()?;
    let pkg_dir = work_dir.join("pkg");
    run(
        "dpkg-deb",
        ["-R".as_ref(), deb_file.as_os_str(), pkg_dir.as_os_str()],
    )?;

    let lib_dir = pkg_dir.join("usr/lib/linewise-desktop");
    ensure_dir(&lib_dir)?;
    copy_license_bundle(root, &pkg_dir.join("usr/share/doc/linewise-desktop"))?;

    // Stage 1 — copy ffmpeg CLI alongside the libraries. We then rerun
    // the dependency walk on it so any libs it pulls in (libfdk-aac etc.)
    // also land in lib_dir.
    let ffmpeg_cli_src = which_path("ffmpeg")?;
    let ffmpeg_cli_dst = lib_dir.join("ffmpeg");
    std::fs::copy(&ffmpeg_cli_src, &ffmpeg_cli_dst)?;
    set_executable(&ffmpeg_cli_dst)?;
    eprintln!("  Copied ffmpeg binary");

    // ffprobe — capture-metadata read-back. Links the same av* libs as ffmpeg
    // (Stage 2 bundles them); its own dep walk runs below alongside ffmpeg's.
    let ffprobe_cli_src = which_path("ffprobe")?;
    let ffprobe_cli_dst = lib_dir.join("ffprobe");
    std::fs::copy(&ffprobe_cli_src, &ffprobe_cli_dst)?;
    set_executable(&ffprobe_cli_dst)?;
    eprintln!("  Copied ffprobe binary");

    // ExifTool (Perl script + lib/) — runs on system perl via the script shebang.
    let exiftool_dist = exiftool_dist()?;
    copy_exiftool_unix(&exiftool_dist, &lib_dir)?;

    // Stage 2 — locate the host's FFmpeg lib directory, then bundle the
    // av* shared libraries plus everything they pull in transitively.
    // On Debian/Ubuntu, ldconfig points us at /usr/lib/x86_64-linux-gnu
    // (or the equivalent multiarch dir) — we use ldd to walk from there.
    let mut bundled: HashSet<String> = HashSet::new();

    let av_lib_dir = locate_av_lib_dir()?;
    eprintln!("Using FFmpeg libs from: {}", av_lib_dir.display());

    for stem in REQUIRED_LIBS {
        let path = find_major_versioned(&av_lib_dir, stem, SharedLibKind::SoMajor)?.with_context(
            || {
                format!(
                    "required library {stem}.so.* not under {}",
                    av_lib_dir.display()
                )
            },
        )?;
        bundle_so_recursive(&path, &lib_dir, &mut bundled)?;
    }
    for stem in OPTIONAL_LIBS {
        if let Some(path) = find_major_versioned(&av_lib_dir, stem, SharedLibKind::SoMajor)? {
            bundle_so_recursive(&path, &lib_dir, &mut bundled)?;
        } else {
            eprintln!("  Skipped {stem} (not in this FFmpeg build)");
        }
    }

    // Walk the ffmpeg CLI's own deps too. libavcodec etc. are already
    // bundled, but the CLI may pull in extra encoder libs (libfdk_aac,
    // libx265) that the av* libs themselves don't link against.
    walk_and_bundle_deps(&ffmpeg_cli_dst, &lib_dir, &mut bundled)?;
    walk_and_bundle_deps(&ffprobe_cli_dst, &lib_dir, &mut bundled)?;

    // Stage 3 — patchelf RPATHs. The main binary lives at
    // /usr/bin/linewise-desktop; the libs are at /usr/lib/linewise-desktop/.
    // $ORIGIN/../lib/linewise-desktop bridges the two without hardcoding
    // the install prefix, so the same .deb works under non-FHS roots.
    if !which("patchelf") {
        bail!("patchelf not found — install via `apt-get install patchelf`");
    }
    let bin_path = pkg_dir.join("usr/bin/linewise-desktop");
    if bin_path.is_file() {
        run(
            "patchelf",
            [
                "--set-rpath".as_ref(),
                "$ORIGIN/../lib/linewise-desktop".as_ref(),
                bin_path.as_os_str(),
            ],
        )?;
        eprintln!("  Patched RPATH on binary");
    }
    run(
        "patchelf",
        [
            "--set-rpath".as_ref(),
            "$ORIGIN".as_ref(),
            ffmpeg_cli_dst.as_os_str(),
        ],
    )?;
    eprintln!("  Patched RPATH on bundled ffmpeg");
    run(
        "patchelf",
        [
            "--set-rpath".as_ref(),
            "$ORIGIN".as_ref(),
            ffprobe_cli_dst.as_os_str(),
        ],
    )?;
    eprintln!("  Patched RPATH on bundled ffprobe");

    // Bundled libs also need $ORIGIN so they can find each other and
    // their transitive deps inside lib_dir. ldd against the bundled
    // dylibs would otherwise still resolve via the system loader, which
    // works on the build host but isn't a guarantee on the target.
    for entry in std::fs::read_dir(&lib_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().into_string().unwrap_or_default();
        if !name.contains(".so") || name == "ffmpeg" {
            continue;
        }
        run(
            "patchelf",
            ["--set-rpath".as_ref(), "$ORIGIN".as_ref(), path.as_os_str()],
        )?;
    }

    // Stage 4 — repack the .deb in place.
    run(
        "dpkg-deb",
        ["-b".as_ref(), pkg_dir.as_os_str(), deb_file.as_os_str()],
    )?;
    eprintln!("Done: {} (with bundled FFmpeg)", deb_file.display());

    std::fs::remove_dir_all(&work_dir).ok();
    Ok(())
}

fn bundle_so_recursive(src: &Path, lib_dir: &Path, bundled: &mut HashSet<String>) -> Result<()> {
    // Resolve symlinks so we copy the real file. ldd reports the
    // symlink target anyway, but read_dir over a lib directory mixes
    // .so / .so.<major> / .so.<full> entries and the major-version
    // entry is often itself a symlink to the full-version one.
    let real =
        std::fs::canonicalize(src).with_context(|| format!("canonicalize {}", src.display()))?;
    let basename = src
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("non-UTF8 .so path: {}", src.display()))?
        .to_owned();
    if !bundled.insert(basename.clone()) {
        return Ok(());
    }

    let dst = lib_dir.join(&basename);
    std::fs::copy(&real, &dst)
        .with_context(|| format!("copy {} → {}", real.display(), dst.display()))?;
    eprintln!("  Copied {basename}");

    walk_and_bundle_deps(&dst, lib_dir, bundled)?;
    Ok(())
}

/// Run ldd against `consumer`, recurse into every dep that isn't on the
/// system denylist. Each ldd line looks like:
///   libavcodec.so.62 => /usr/lib/x86_64-linux-gnu/libavcodec.so.62 (0x...)
///   linux-vdso.so.1 (0x...)              # virtual, skip
///   /lib64/ld-linux-x86-64.so.2 (0x...)  # loader, skip
fn walk_and_bundle_deps(
    consumer: &Path,
    lib_dir: &Path,
    bundled: &mut HashSet<String>,
) -> Result<()> {
    let out = capture("ldd", [consumer.as_os_str()])?;
    for line in out.lines() {
        let Some(dep) = parse_ldd_line(line) else {
            continue;
        };
        if is_system_lib(&dep.soname) {
            continue;
        }
        let Some(path) = dep.resolved else { continue };
        bundle_so_recursive(&path, lib_dir, bundled)?;
    }
    Ok(())
}

#[derive(Debug)]
struct LddLine {
    soname: String,
    resolved: Option<PathBuf>,
}

fn parse_ldd_line(line: &str) -> Option<LddLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    // "linux-vdso.so.1 (0x...)" — virtual, no resolution.
    // "libavcodec.so.62 => /usr/lib/x86_64-linux-gnu/libavcodec.so.62 (0x...)"
    // "/lib64/ld-linux-x86-64.so.2 (0x...)" — absolute, no =>.
    if let Some((left, right)) = line.split_once("=>") {
        let soname = left.trim().to_owned();
        let resolved = right.split_whitespace().next();
        let resolved = resolved.and_then(|p| {
            if p == "(0x" || p.starts_with("(0x") || p == "not" || p.is_empty() {
                None
            } else {
                Some(PathBuf::from(p))
            }
        });
        return Some(LddLine { soname, resolved });
    }
    // No `=>` — either a virtual entry (linux-vdso) or an absolute
    // path that ldd printed without resolution.
    if let Some(path) = line.split_whitespace().next() {
        if path.starts_with('/') {
            let pb = PathBuf::from(path);
            let soname = pb
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            return Some(LddLine {
                soname,
                resolved: Some(pb),
            });
        }
        return Some(LddLine {
            soname: path.to_owned(),
            resolved: None,
        });
    }
    None
}

fn is_system_lib(soname: &str) -> bool {
    SYSTEM_DENYLIST
        .iter()
        .any(|prefix| soname.starts_with(prefix))
}

/// Search the standard multiarch lib dirs for the av* libraries.
/// On Debian/Ubuntu these live under /usr/lib/x86_64-linux-gnu; on
/// Fedora/Arch they're directly under /usr/lib64 or /usr/lib. We probe
/// the common locations and take the first that has libavcodec.so.<major>.
fn locate_av_lib_dir() -> Result<PathBuf> {
    let candidates = [
        PathBuf::from("/usr/lib/x86_64-linux-gnu"),
        PathBuf::from("/usr/lib64"),
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/local/lib"),
    ];
    for dir in candidates {
        if !dir.is_dir() {
            continue;
        }
        if find_major_versioned(&dir, "libavcodec", SharedLibKind::SoMajor)?.is_some() {
            return Ok(dir);
        }
    }
    bail!("could not locate FFmpeg libs (libavcodec.so.*) under standard /usr/lib paths")
}

fn which_path(program: &str) -> Result<PathBuf> {
    let out = capture("which", [program])?;
    let path = PathBuf::from(out.trim());
    if path.as_os_str().is_empty() {
        bail!("`which {program}` returned no path");
    }
    Ok(path)
}

fn tempdir() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    let unique = format!(
        "linewise-xtask-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let dir = base.join(unique);
    ensure_dir(&dir)?;
    Ok(dir)
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
