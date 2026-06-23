//! Capture metadata (io.visionlab.* schema) embedding into the QuickTime `udta`
//! tags of an MP4/MOV via **ExifTool**, so the backend's ffprobe extraction
//! lifts it into `documents.metadata.capture`.
//!
//! The desktop **writes** the user-entered fields; the backend is the single
//! **reader/extractor**. Device `make`/`model` go into the standard atoms
//! (`©mak`/`©mod`); the linewise-specific fields go under per-field
//! `io.visionlab.*` keys in the QuickTime `Keys` atom, registered via an
//! ExifTool `-config` UserDefined block. ExifTool writes them under
//! `com.apple.quicktime.io.visionlab.*`; the backend extractor strips that
//! prefix before matching — verified round-trip.
//!
//! ExifTool (not ffmpeg) is used deliberately: ffmpeg's `use_metadata_tags`
//! handling of custom keys is unreliable; ExifTool writes them cleanly.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ffmpeg_util::hidden_command;

/// io.visionlab container schema version this client writes.
pub const CAPTURE_SCHEMA_VERSION: i32 = 1;

/// ExifTool UserDefined config registering the `io.visionlab.*` keys under the
/// QuickTime `Keys` table, so they can be written by their `VisionLab*` names.
/// Materialized to a temp file and passed via `-config` (must be a startup arg).
const VISIONLAB_CONFIG: &str = r#"%Image::ExifTool::UserDefined = (
  'Image::ExifTool::QuickTime::Keys' => {
    'io.visionlab.schema'   => { Name => 'VisionLabSchema' },
    'io.visionlab.country'  => { Name => 'VisionLabCountry' },
    'io.visionlab.city'     => { Name => 'VisionLabCity' },
    'io.visionlab.site'     => { Name => 'VisionLabSite' },
    'io.visionlab.station'  => { Name => 'VisionLabStation' },
    'io.visionlab.operator' => { Name => 'VisionLabOperator' },
    'io.visionlab.action'   => { Name => 'VisionLabAction' },
    'io.visionlab.fov'      => { Name => 'VisionLabFov' },
  },
);
1;
"#;

/// User-entered capture metadata. Held in memory only (not persisted to SQLite);
/// embedded into the file before upload. `None` fields are omitted entirely (the
/// backend distinguishes "absent" from "blank").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMetadata {
    pub country: Option<String>,
    pub city: Option<String>,
    pub site: Option<String>,
    pub station: Option<String>,
    /// Canonical 3-digit operator code (e.g. "001"); validate via [`canonicalize_operator`].
    pub operator: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub fov: Option<i32>,
    pub action: Option<String>,
}

impl CaptureMetadata {
    /// True when no field is set — embedding is skipped for empty metadata.
    pub fn is_empty(&self) -> bool {
        self.country.is_none()
            && self.city.is_none()
            && self.site.is_none()
            && self.station.is_none()
            && self.operator.is_none()
            && self.make.is_none()
            && self.model.is_none()
            && self.fov.is_none()
            && self.action.is_none()
    }
}

/// Normalize an operator code the same way the backend does: 1–3 ASCII digits,
/// left-padded with zeros to the canonical 3-digit form ("1"/"01" → "001").
/// Non-numeric or >3-digit input is rejected.
pub fn canonicalize_operator(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    if (1..=3).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit()) {
        Ok(format!("{t:0>3}"))
    } else {
        Err(format!("Operator must be 1-3 digits: '{raw}'"))
    }
}

/// Validate FOV in plausible degree range (matches the backend's `Fov`: 1–360).
pub fn validate_fov(n: i32) -> Result<i32, String> {
    if (1..=360).contains(&n) {
        Ok(n)
    } else {
        Err(format!("FOV must be within 1-360 degrees: {n}"))
    }
}

/// Error embedding capture metadata via ExifTool.
#[derive(Debug, thiserror::Error)]
pub enum CaptureEmbedError {
    #[error("exiftool failed: {0}")]
    Exiftool(String),
    /// The exiftool binary couldn't be found/launched. Distinct from a write
    /// failure so the adaptive embed does NOT fall back to a local-copy retry
    /// (the copy would hit the same missing binary) — it surfaces immediately.
    #[error("exiftool binary not found")]
    ExiftoolNotFound,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid path: {0}")]
    InvalidPath(String),
}

/// Where the tagged bytes ended up.
pub struct EmbedOutcome {
    /// The file to upload (the tagged original, or a tagged local copy).
    pub path: PathBuf,
    /// True when `path` is a throwaway local copy the caller must clean up
    /// (the in-place write to the source wasn't possible — read-only / full /
    /// removable media). False when the source file itself was tagged in place
    /// (never delete it).
    pub is_temp_copy: bool,
}

/// Materialize the bundled UserDefined config to a stable temp path and return it.
fn config_path() -> Result<PathBuf, CaptureEmbedError> {
    let dir = std::env::temp_dir().join("linewise-capture");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("visionlab.config");
    // Rewrite each call — cheap, and self-heals if the temp file was cleared.
    std::fs::write(&path, VISIONLAB_CONFIG)?;
    Ok(path)
}

/// Embed `meta` into the file, adapting to the source medium:
///
/// 1. **In place** on `input` — one full-file rewrite (MP4 metadata always forces
///    one; moov sits ahead of mdat), streams preserved byte-for-byte (no re-mux),
///    via exiftool's tmp-then-atomic-rename. Leaves the source self-describing, so
///    a re-add reads the tags back. Returns `is_temp_copy: false`.
/// 2. **Fallback** when the in-place write fails (read-only or full removable
///    media — USB / SD card): copy `input` to `scratch_dir` (a local volume) and
///    tag the copy instead. The source is left untouched; the caller uploads the
///    copy and must delete it. Returns `is_temp_copy: true`.
///
/// The uploaded file is tagged either way, so a later server-side backfill can
/// re-extract the values from the GCS object regardless of which path ran.
/// `ExiftoolNotFound` is NOT retried via the fallback (the copy would hit the
/// same missing binary). Blocking (a multi-GB clip is a multi-second rewrite) —
/// call from `spawn_blocking`.
pub fn embed_capture_metadata_blocking(
    input: &Path,
    meta: &CaptureMetadata,
    scratch_dir: &Path,
) -> Result<EmbedOutcome, CaptureEmbedError> {
    match write_tags_in_place(input, meta) {
        Ok(()) => Ok(EmbedOutcome {
            path: input.to_path_buf(),
            is_temp_copy: false,
        }),
        // Missing binary: a local-copy retry would fail identically — surface now.
        Err(CaptureEmbedError::ExiftoolNotFound) => Err(CaptureEmbedError::ExiftoolNotFound),
        // Source not writable / out of space (USB, SD, read-only mount): tag a
        // local copy instead and upload that.
        Err(in_place_err) => {
            tracing::warn!(
                input = %input.display(),
                "[capture] in-place embed failed ({in_place_err}); falling back to a local tagged copy"
            );
            std::fs::create_dir_all(scratch_dir)?;
            let filename = input
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "clip.mp4".to_string());
            let copy = scratch_dir.join(format!("vlmeta_{filename}"));
            std::fs::copy(input, &copy)?;
            write_tags_in_place(&copy, meta)?;
            Ok(EmbedOutcome {
                path: copy,
                is_temp_copy: true,
            })
        }
    }
}

/// Run exiftool to write the capture tags into `target`, overwriting it in place
/// (`-overwrite_original` = tmp-then-atomic-rename). Used both for the direct
/// in-place attempt on the source and for tagging the fallback local copy.
fn write_tags_in_place(target: &Path, meta: &CaptureMetadata) -> Result<(), CaptureEmbedError> {
    let cfg = config_path()?;
    let cfg_str = cfg
        .to_str()
        .ok_or_else(|| CaptureEmbedError::InvalidPath(cfg.display().to_string()))?;
    let target_str = target
        .to_str()
        .ok_or_else(|| CaptureEmbedError::InvalidPath(target.display().to_string()))?;

    // `-config` must be the first arg. ExifTool writes the registered VisionLab*
    // names into the io.visionlab.* Keys; Make/Model into the standard atoms.
    let mut args: Vec<String> = vec![
        "-config".into(),
        cfg_str.into(),
        "-overwrite_original".into(),
        format!("-VisionLabSchema={CAPTURE_SCHEMA_VERSION}"),
    ];
    let fov_str = meta.fov.map(|n| n.to_string());
    {
        let mut push = |flag: &str, value: Option<&str>| {
            if let Some(v) = value
                && !v.trim().is_empty()
            {
                args.push(format!("-{flag}={v}"));
            }
        };
        push("Make", meta.make.as_deref());
        push("Model", meta.model.as_deref());
        push("VisionLabCountry", meta.country.as_deref());
        push("VisionLabCity", meta.city.as_deref());
        push("VisionLabSite", meta.site.as_deref());
        push("VisionLabStation", meta.station.as_deref());
        push("VisionLabOperator", meta.operator.as_deref());
        push("VisionLabAction", meta.action.as_deref());
        push("VisionLabFov", fov_str.as_deref());
    }
    args.push(target_str.into());

    // One-shot exiftool invocation (NOT the persistent-process crate): `-config`
    // is a startup-only option and is silently ignored inside the crate's
    // `-stay_open` session, so the custom io.visionlab.* keys never get written.
    // A plain one-shot honours `-config` (verified). `hidden_command` suppresses
    // the console window on Windows, mirroring the ffmpeg image-strip path.
    let result = hidden_command(resolve_exiftool_binary())
        .args(&args)
        .output();
    match result {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(CaptureEmbedError::Exiftool(
            String::from_utf8_lossy(&out.stderr).to_string(),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(CaptureEmbedError::ExiftoolNotFound)
        }
        Err(e) => Err(CaptureEmbedError::Io(e)),
    }
}

/// Write the capture tags into `input` **in place only** — no copy fallback.
/// Used at Save time: the whole point is to tag the user's own file so a re-add
/// reads the values back, which a local copy can't provide. On read-only / full
/// media this fails and the caller keeps the values in memory; the upload path's
/// adaptive [`embed_capture_metadata_blocking`] then tags a copy instead.
/// Blocking — call from `spawn_blocking`.
pub fn embed_in_place_blocking(
    input: &Path,
    meta: &CaptureMetadata,
) -> Result<(), CaptureEmbedError> {
    write_tags_in_place(input, meta)
}

/// Parse capture metadata out of a clip's probe tags (the `(key, value)` pairs
/// ffprobe reports in `format.tags`), so a re-added — or vendor-pre-tagged — file
/// shows its embedded values without re-entry. Keys may carry the
/// `com.apple.quicktime.` prefix exiftool writes under; it is stripped before
/// matching. Returns `None` when no io.visionlab/make/model tag is present.
pub fn parse_capture_from_tags(tags: &[(String, String)]) -> Option<CaptureMetadata> {
    let mut meta = CaptureMetadata::default();
    let mut found = false;
    for (raw_key, value) in tags {
        let v = value.trim();
        if v.is_empty() {
            continue;
        }
        // Normalize: drop the QuickTime container prefix and lowercase.
        let key = raw_key
            .trim()
            .trim_start_matches("com.apple.quicktime.")
            .to_ascii_lowercase();
        let owned = v.to_string();
        match key.as_str() {
            "io.visionlab.country" => meta.country = Some(owned),
            "io.visionlab.city" => meta.city = Some(owned),
            "io.visionlab.site" => meta.site = Some(owned),
            "io.visionlab.station" => meta.station = Some(owned),
            "io.visionlab.operator" => meta.operator = Some(owned),
            "io.visionlab.action" => meta.action = Some(owned),
            "io.visionlab.fov" => meta.fov = v.parse::<i32>().ok(),
            "make" => meta.make = Some(owned),
            "model" => meta.model = Some(owned),
            _ => continue,
        }
        found = true;
    }
    (found && !meta.is_empty()).then_some(meta)
}

/// Read io.visionlab capture metadata back from a file's QuickTime tags via a
/// local `ffprobe -show_entries format_tags` (reads only the moov atom — fast
/// even for multi-GB clips, no media scan). Returns `None` when ffprobe is
/// unavailable, the file is unreadable, or it carries no capture tags. Used to
/// recover state on restart (the in-memory maps are lost) and to recognize
/// vendor-pre-tagged files. Blocking — call from `spawn_blocking`.
pub fn read_embedded_capture(path: &Path) -> Option<CaptureMetadata> {
    let out = hidden_command(resolve_ffprobe_binary())
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_entries",
            "format_tags",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let tags_obj = json.get("format")?.get("tags")?.as_object()?;
    let tags: Vec<(String, String)> = tags_obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();
    parse_capture_from_tags(&tags)
}

/// Resolve the ffprobe binary, preferring a bundled copy (next to ffmpeg) over
/// system PATH. Mirrors [`resolve_exiftool_binary`].
fn resolve_ffprobe_binary() -> std::ffi::OsString {
    use std::ffi::OsString;
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(target_os = "macos")]
        if let Some(parent) = exe.parent().and_then(|p| p.parent()) {
            let c = parent.join("Resources").join("ffprobe");
            if c.exists() {
                return c.into_os_string();
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(dir) = exe.parent() {
            let c = dir.join("ffprobe.exe");
            if c.exists() {
                return c.into_os_string();
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(dir) = exe.parent() {
            for rel in ["ffprobe", "../lib/linewise-desktop/ffprobe"] {
                let c = dir.join(rel);
                if c.exists() {
                    return c.into_os_string();
                }
            }
        }
    }
    OsString::from("ffprobe")
}

/// Resolve the exiftool binary, preferring a bundled copy over system PATH.
/// (Bundling is wired in xtask; until then this falls back to PATH `exiftool`.)
fn resolve_exiftool_binary() -> std::ffi::OsString {
    use std::ffi::OsString;
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(target_os = "macos")]
        if let Some(parent) = exe.parent().and_then(|p| p.parent()) {
            let c = parent.join("Resources").join("exiftool");
            if c.exists() {
                return c.into_os_string();
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(dir) = exe.parent() {
            let c = dir.join("exiftool.exe");
            if c.exists() {
                return c.into_os_string();
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(dir) = exe.parent() {
            for rel in ["exiftool", "../lib/linewise-desktop/exiftool"] {
                let c = dir.join(rel);
                if c.exists() {
                    return c.into_os_string();
                }
            }
        }
    }
    OsString::from("exiftool")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_canonicalizes_to_three_digits() {
        assert_eq!(canonicalize_operator("1").unwrap(), "001");
        assert_eq!(canonicalize_operator("01").unwrap(), "001");
        assert_eq!(canonicalize_operator("001").unwrap(), "001");
        assert_eq!(canonicalize_operator(" 12 ").unwrap(), "012");
        assert!(canonicalize_operator("AB").is_err());
        assert!(canonicalize_operator("1000").is_err());
        assert!(canonicalize_operator("").is_err());
    }

    #[test]
    fn fov_range_enforced() {
        assert_eq!(validate_fov(143).unwrap(), 143);
        assert_eq!(validate_fov(1).unwrap(), 1);
        assert_eq!(validate_fov(360).unwrap(), 360);
        assert!(validate_fov(0).is_err());
        assert!(validate_fov(361).is_err());
    }

    #[test]
    fn parse_from_tags_strips_prefix_and_typed_fov() {
        let tags = vec![
            (
                "com.apple.quicktime.io.visionlab.country".to_string(),
                "Thailand".to_string(),
            ),
            ("io.visionlab.operator".to_string(), "001".to_string()),
            (
                "com.apple.quicktime.io.visionlab.fov".to_string(),
                "143".to_string(),
            ),
            ("make".to_string(), "DJI".to_string()),
            ("model".to_string(), "Osmo Nano".to_string()),
            ("major_brand".to_string(), "mp42".to_string()), // unrelated → ignored
            ("io.visionlab.city".to_string(), "  ".to_string()), // blank → skipped
        ];
        let m = parse_capture_from_tags(&tags).expect("should parse");
        assert_eq!(m.country.as_deref(), Some("Thailand"));
        assert_eq!(m.operator.as_deref(), Some("001"));
        assert_eq!(m.fov, Some(143));
        assert_eq!(m.make.as_deref(), Some("DJI"));
        assert_eq!(m.model.as_deref(), Some("Osmo Nano"));
        assert_eq!(m.city, None);
    }

    #[test]
    fn parse_from_tags_none_when_absent() {
        let tags = vec![
            ("major_brand".to_string(), "mp42".to_string()),
            ("encoder".to_string(), "Lavf".to_string()),
        ];
        assert!(parse_capture_from_tags(&tags).is_none());
    }

    #[test]
    fn empty_detection() {
        assert!(CaptureMetadata::default().is_empty());
        let m = CaptureMetadata {
            site: Some("PackagingParts01".into()),
            ..Default::default()
        };
        assert!(!m.is_empty());
    }

    /// Round-trip: embed via ExifTool → ffprobe asserts the tags landed (under the
    /// `com.apple.quicktime.io.visionlab.*` prefix the backend strips). Requires
    /// exiftool + ffmpeg + ffprobe on PATH; ignored so CI without them stays green.
    /// Run: `cargo test -p lw-core --lib capture::tests::roundtrip -- --ignored`.
    #[test]
    #[ignore]
    fn roundtrip() {
        let dir = std::env::temp_dir().join("linewise-capture-test");
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("base.mp4");
        let gen_out = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=10",
            ])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&base)
            .output()
            .expect("ffmpeg generate");
        assert!(
            gen_out.status.success(),
            "gen failed: {}",
            String::from_utf8_lossy(&gen_out.stderr)
        );

        let meta = CaptureMetadata {
            country: Some("Thailand".into()),
            site: Some("AutomotiveSiliconeParts01".into()),
            operator: Some("001".into()),
            make: Some("DJI".into()),
            model: Some("Osmo Nano".into()),
            fov: Some(143),
            action: Some("Pressing piston rings".into()),
            ..Default::default()
        };
        let outcome = embed_capture_metadata_blocking(&base, &meta, &dir).expect("embed");
        // Local temp file is writable, so the in-place path should run.
        assert!(
            !outcome.is_temp_copy,
            "expected in-place tagging on local temp"
        );

        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_entries",
                "format_tags",
            ])
            .arg(&outcome.path)
            .output()
            .expect("ffprobe");
        let out = String::from_utf8_lossy(&probe.stdout);
        assert!(
            out.contains("io.visionlab.country"),
            "missing country: {out}"
        );
        assert!(out.contains("Thailand"), "missing value: {out}");
        assert!(
            out.contains("io.visionlab.operator"),
            "missing operator: {out}"
        );
        assert!(out.contains("io.visionlab.fov"), "missing fov: {out}");
        assert!(out.contains("\"make\""), "missing make: {out}");

        // Restart-recovery path: read the tags back from the file locally.
        let recovered = read_embedded_capture(&outcome.path).expect("read back");
        assert_eq!(recovered.country.as_deref(), Some("Thailand"));
        assert_eq!(recovered.operator.as_deref(), Some("001"));
        assert_eq!(recovered.fov, Some(143));
        assert_eq!(recovered.make.as_deref(), Some("DJI"));
        assert_eq!(recovered.model.as_deref(), Some("Osmo Nano"));
    }
}
