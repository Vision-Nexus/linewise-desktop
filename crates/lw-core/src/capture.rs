//! Capture metadata (io.visionlab.* schema) embedding into the QuickTime `udta`
//! tags of an MP4/MOV, so the backend's ffprobe extraction lifts it into
//! `documents.metadata.capture`.
//!
//! The desktop **writes** the user-entered fields into the file; the backend is
//! the single **reader/extractor**. Device `make`/`model` go into the standard
//! atoms (`©mak`/`©mod`); the linewise-specific fields go under per-field
//! `io.visionlab.*` keys. Writing uses the bundled ffmpeg CLI with
//! `-movflags use_metadata_tags` — without that flag ffmpeg drops the
//! non-standard keys.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::desensitize::{hidden_command, resolve_ffmpeg_binary};

/// io.visionlab container schema version this client writes.
pub const CAPTURE_SCHEMA_VERSION: i32 = 1;

/// The single udta key namespace prefix the backend reads.
const VL_PREFIX: &str = "io.visionlab.";

/// User-entered capture metadata. Held in memory only (not persisted to SQLite);
/// embedded into the file at upload-confirm time. `None` fields are omitted from
/// the file entirely (the backend distinguishes "absent" from "blank").
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

/// Error embedding capture metadata via ffmpeg.
#[derive(Debug, thiserror::Error)]
pub enum CaptureEmbedError {
    #[error("ffmpeg failed: {0}")]
    FfmpegFailed(String),
    #[error("ffmpeg binary not available")]
    FfmpegNotAvailable,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Embed `meta` into a copy of `input` and return the tagged file path.
///
/// Stream-copy remux (no re-encode) writing make/model + `io.visionlab.*` with
/// `-movflags use_metadata_tags`. The tagged copy becomes the upload source, so
/// the caller must re-hash it before dedup / create-document (its bytes differ
/// from the original). Blocking — call from `spawn_blocking`.
pub fn embed_capture_metadata_blocking(
    input: &Path,
    meta: &CaptureMetadata,
) -> Result<PathBuf, CaptureEmbedError> {
    let temp_dir = std::env::temp_dir().join("linewise-capture");
    std::fs::create_dir_all(&temp_dir)?;
    let filename = input
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip.mp4".to_string());
    let output = temp_dir.join(format!("vlmeta_{filename}"));

    let mut cmd = hidden_command(resolve_ffmpeg_binary());
    cmd.args(["-y", "-i"])
        .arg(input)
        // Preserve existing container metadata, then layer ours on top.
        .args([
            "-map_metadata",
            "0",
            "-c",
            "copy",
            "-movflags",
            "use_metadata_tags",
        ]);

    // Device → standard atoms (©mak/©mod).
    push_meta(&mut cmd, "make", meta.make.as_deref());
    push_meta(&mut cmd, "model", meta.model.as_deref());
    // Schema + linewise payload → io.visionlab.* keys.
    push_meta(
        &mut cmd,
        &vl("schema"),
        Some(&CAPTURE_SCHEMA_VERSION.to_string()),
    );
    push_meta(&mut cmd, &vl("country"), meta.country.as_deref());
    push_meta(&mut cmd, &vl("city"), meta.city.as_deref());
    push_meta(&mut cmd, &vl("site"), meta.site.as_deref());
    push_meta(&mut cmd, &vl("station"), meta.station.as_deref());
    push_meta(&mut cmd, &vl("operator"), meta.operator.as_deref());
    push_meta(&mut cmd, &vl("action"), meta.action.as_deref());
    push_meta(
        &mut cmd,
        &vl("fov"),
        meta.fov.map(|n| n.to_string()).as_deref(),
    );

    cmd.arg(&output);

    match cmd.output() {
        Ok(out) if out.status.success() => Ok(output),
        Ok(out) => Err(CaptureEmbedError::FfmpegFailed(
            String::from_utf8_lossy(&out.stderr).to_string(),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(CaptureEmbedError::FfmpegNotAvailable)
        }
        Err(e) => Err(CaptureEmbedError::Io(e)),
    }
}

fn vl(field: &str) -> String {
    format!("{VL_PREFIX}{field}")
}

/// Push `-metadata key=value` only when value is present and non-blank.
fn push_meta(cmd: &mut std::process::Command, key: &str, value: Option<&str>) {
    if let Some(v) = value
        && !v.trim().is_empty()
    {
        cmd.arg("-metadata").arg(format!("{key}={v}"));
    }
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
    fn empty_detection() {
        assert!(CaptureMetadata::default().is_empty());
        let m = CaptureMetadata {
            site: Some("PackagingParts01".into()),
            ..Default::default()
        };
        assert!(!m.is_empty());
    }

    /// Round-trip: embed → ffprobe asserts the tags landed. Requires ffmpeg +
    /// ffprobe on PATH; ignored by default so CI without them stays green.
    /// Run with `cargo test -p lw-core -- --ignored capture_embed_roundtrip`.
    #[test]
    #[ignore]
    fn capture_embed_roundtrip() {
        let dir = std::env::temp_dir().join("linewise-capture-test");
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("base.mp4");
        // Generate a tiny test clip.
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
        let tagged = embed_capture_metadata_blocking(&base, &meta).expect("embed");

        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_entries",
                "format_tags",
            ])
            .arg(&tagged)
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
    }
}
