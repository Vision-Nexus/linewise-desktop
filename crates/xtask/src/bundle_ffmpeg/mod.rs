use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

mod linux;
mod macos;
mod windows;

/// FFmpeg shared libraries we link against directly. The order matters
/// only insofar as dependents come after their dependencies, which keeps
/// the dependency walk's debug output readable.
const REQUIRED_LIBS: &[&str] = &[
    "libavutil",
    "libavcodec",
    "libavformat",
    "libswscale",
    "libswresample",
    "libavfilter",
    "libavdevice",
];

/// libpostproc is GPL-only and isn't shipped by every FFmpeg build.
const OPTIONAL_LIBS: &[&str] = &["libpostproc"];

pub fn run(root: &Path, target: &str, create_dmg: bool) -> Result<()> {
    let host = HostOs::detect();
    match (host, target) {
        (HostOs::MacOs, t) if t.ends_with("-apple-darwin") => macos::bundle(root, t, create_dmg),
        (HostOs::Linux, t) if t.contains("-linux-") => linux::bundle(root, t),
        (HostOs::Windows, t) if t.contains("-windows-") => windows::bundle(root, t),
        (host, t) => bail!(
            "host {host:?} cannot bundle for target {t} — run xtask on the same OS as the target"
        ),
    }
}

#[derive(Debug, Clone, Copy)]
enum HostOs {
    MacOs,
    Linux,
    Windows,
}

impl HostOs {
    fn detect() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            panic!("unsupported host OS for xtask bundle-ffmpeg")
        }
    }
}

/// Resolve the ExifTool distribution to bundle. We require `EXIFTOOL_DIST` to
/// point at an **extracted official distribution** (the `exiftool` script + its
/// `lib/` directory, as shipped in `Image-ExifTool-NN.NN.tar.gz`). That layout is
/// self-locating — the script adds `$exeDir/lib` to `@INC` — so it runs anywhere
/// with system Perl, unlike a Homebrew/apt install whose script hardcodes an
/// absolute, build-host `@INC` path. On Windows, point it at the standalone build
/// (containing `exiftool.exe`, which embeds its own Perl — no system Perl needed).
///
/// Hard error when unset: shipping a release that can't embed capture metadata is
/// worse than failing the build. CI must provide it (download + extract the
/// pinned tarball / Windows zip from https://exiftool.org).
pub(crate) fn exiftool_dist() -> Result<PathBuf> {
    let dir = std::env::var("EXIFTOOL_DIST").context(
        "EXIFTOOL_DIST not set — point it at an extracted ExifTool distribution \
         (the `exiftool` script + `lib/` on macOS/Linux, or the directory holding \
         the standalone `exiftool.exe` on Windows). Download from https://exiftool.org.",
    )?;
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        bail!("EXIFTOOL_DIST is not a directory: {}", dir.display());
    }
    Ok(dir)
}

/// macOS/Linux: copy the ExifTool script + its `lib/` into `dst_dir`, preserving
/// the script↔lib sibling layout so the script self-locates its modules at
/// runtime via system Perl. Returns the path to the copied `exiftool` script.
pub(crate) fn copy_exiftool_unix(dist: &Path, dst_dir: &Path) -> Result<PathBuf> {
    crate::cmd::ensure_dir(dst_dir)?;
    let script_src = dist.join("exiftool");
    if !script_src.is_file() {
        bail!(
            "no `exiftool` script under EXIFTOOL_DIST ({}) — expected an extracted \
             official distribution",
            dist.display()
        );
    }
    let lib_src = dist.join("lib");
    if !lib_src.is_dir() {
        bail!(
            "no `lib/` directory under EXIFTOOL_DIST ({}) — the script can't find its \
             modules without it",
            dist.display()
        );
    }
    let script_dst = dst_dir.join("exiftool");
    std::fs::copy(&script_src, &script_dst)
        .with_context(|| format!("copy exiftool script: {}", script_src.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_dst)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&script_dst, perms)
            .with_context(|| format!("chmod +x {}", script_dst.display()))?;
    }
    copy_dir_all(&lib_src, &dst_dir.join("lib"))?;
    eprintln!("  Copied exiftool + lib/ → {}", dst_dir.display());
    Ok(script_dst)
}

/// Copy `THIRD_PARTY_LICENSES.md` and the `NOTICES/` directory into `dst`.
/// Every platform bundler ships these so the About pane has a local copy.
pub(crate) fn copy_license_bundle(root: &Path, dst: &Path) -> Result<()> {
    crate::cmd::ensure_dir(dst)?;
    let licenses_md = root.join("THIRD_PARTY_LICENSES.md");
    std::fs::copy(&licenses_md, dst.join("THIRD_PARTY_LICENSES.md"))
        .with_context(|| format!("copy {} → {}", licenses_md.display(), dst.display()))?;
    copy_dir_all(&root.join("NOTICES"), &dst.join("NOTICES"))?;
    eprintln!(
        "  Copied THIRD_PARTY_LICENSES.md + NOTICES/ → {}",
        dst.display()
    );
    Ok(())
}

pub(crate) fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    crate::cmd::ensure_dir(dst)?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read_dir {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} → {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Used by macOS/Linux to find the major-versioned shared library by
/// stem (e.g. `libavcodec` → `libavcodec.62.dylib` or `libavcodec.so.62`).
/// Returns the path that should be copied into the bundle.
pub(crate) fn find_major_versioned(
    lib_dir: &Path,
    stem: &str,
    extension_hint: SharedLibKind,
) -> Result<Option<PathBuf>> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(lib_dir)
        .with_context(|| format!("read_dir {}", lib_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| extension_hint.matches(p, stem))
        .collect();
    candidates.sort();
    Ok(candidates.into_iter().next())
}

#[derive(Clone, Copy)]
pub(crate) enum SharedLibKind {
    /// macOS: `libfoo.<major>.dylib`, not `libfoo.dylib` and not `libfoo.<major>.<minor>.<patch>.dylib`.
    Dylib,
    /// Linux: `libfoo.so.<major>`, not the bare `libfoo.so` symlink.
    SoMajor,
}

impl SharedLibKind {
    pub(crate) fn matches(self, path: &Path, stem: &str) -> bool {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        match self {
            Self::Dylib => {
                // libavcodec.62.dylib — exactly one component between the
                // stem and the .dylib extension. We strip the prefix and
                // suffix with `strip_*` to avoid index math that would
                // panic on the bare `libavcodec.dylib` symlink, where
                // there's nothing between the two.
                let after_stem = match name.strip_prefix(&format!("{stem}.")) {
                    Some(s) => s,
                    None => return false,
                };
                let middle = match after_stem.strip_suffix(".dylib") {
                    Some(s) => s,
                    None => return false,
                };
                !middle.is_empty() && !middle.contains('.')
            }
            Self::SoMajor => {
                // libavcodec.so.62 — major-versioned symlink, real file is
                // libavcodec.so.62.x.y. We pick the .so.<major> form because
                // that's what LC_NEEDED records.
                let Some(tail) = name.strip_prefix(&format!("{stem}.so.")) else {
                    return false;
                };
                !tail.is_empty() && !tail.contains('.')
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_matches_major_versioned_only() {
        let kind = SharedLibKind::Dylib;
        // Major-versioned: yes.
        assert!(kind.matches(Path::new("libavcodec.62.dylib"), "libavcodec"));
        // Bare symlink: no — this is the case that previously panicked.
        assert!(!kind.matches(Path::new("libavcodec.dylib"), "libavcodec"));
        // Fully-versioned: no — picking this would copy the wrong basename.
        assert!(!kind.matches(Path::new("libavcodec.62.0.100.dylib"), "libavcodec"));
        // Wrong stem.
        assert!(!kind.matches(Path::new("libavformat.62.dylib"), "libavcodec"));
        // Different extension.
        assert!(!kind.matches(Path::new("libavcodec.62.so"), "libavcodec"));
    }

    #[test]
    fn so_major_matches_major_versioned_only() {
        let kind = SharedLibKind::SoMajor;
        assert!(kind.matches(Path::new("libavcodec.so.62"), "libavcodec"));
        assert!(!kind.matches(Path::new("libavcodec.so"), "libavcodec"));
        assert!(!kind.matches(Path::new("libavcodec.so.62.0.100"), "libavcodec"));
        assert!(!kind.matches(Path::new("libavformat.so.62"), "libavcodec"));
    }
}
