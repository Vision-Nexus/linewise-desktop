//! Tier-2 PDQ perceptual near-duplicate hashing for the upload path.
//!
//! Exact-content dedup (md5 / crc32c / sha256-head, see [`crate::dedup`]) misses
//! a re-encode or a metadata edit: identical footage whose bytes differ. PDQ
//! (Meta's perceptual image hash) closes that gap by hashing a handful of
//! decoded frames; two near-identical videos produce 256-bit hashes within a
//! small Hamming distance even after re-muxing.
//!
//! This module mirrors the server PDQ sidecar (`scripts/pdq/http_server.py`)
//! and the browser path (vision-lab-platform) so all three produce the SAME
//! 64-hex hash for the same frame:
//!   - sample the SAME fixed timestamps ([`PDQ_TIMES`]),
//!   - decode each via the bundled ffmpeg CLI with the SAME argv
//!     (`-ss <t> -i <file> -frames:v 1 -vf scale=512:512 -pix_fmt rgb24 ...`),
//!   - hash with Meta PDQ and render the 256-bit digest as 64 lowercase hex.
//!
//! ## Bit-exactness is load-bearing
//! The server stores each hash as `bit(256)` and matches via raw Hamming
//! distance. A hash that differs from the browser/sidecar hash space — wrong
//! bit order, a different PDQ variant, a different ffmpeg resize kernel — never
//! matches, silently. [`PDQ_ENABLED`] was flipped to `true` after the desktop
//! hash was verified to land in the server hash space end-to-end against a live
//! backend. An automated "desktop hash == sidecar hash, distance 0" CI gate
//! (see the still-`#[ignore]`d `pdq_hash_bit_exact_with_sidecar` test) remains
//! the recommended regression guard.
//!
//! ## Soft-skip
//! PDQ is strictly additive. Any failure (no bundled ffmpeg, decode/seek error,
//! short read, hash failure) drops the affected frame — or the whole bag — to
//! empty and the upload proceeds on exact digests alone. PDQ never blocks an
//! upload; only a positive *server* near-duplicate verdict does (handled in
//! [`crate::upload`]).

use crate::models::PdqFrameWire;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Output;

/// Master kill-switch (mirrors the browser `STUB_MODE` and the server
/// `PDQ_ENABLED`). Enabled after the desktop hash was verified end-to-end
/// against a live backend (see the module header). While `false`,
/// [`compute_pdq_frames`] returns empty and the upload path sends no
/// `pdq_frames`, so behaviour is byte-identical to the pre-PDQ client.
pub const PDQ_ENABLED: bool = true;

/// Fixed seek points in seconds. Must match the sidecar
/// (`DEFAULT_TIMES = [1, 5, 10, 15, 20]`) and the browser so query frames land
/// in the same hash space as stored frames.
const PDQ_TIMES: [u32; 5] = [1, 5, 10, 15, 20];

/// Square frame edge fed to PDQ. The ffmpeg `scale=512:512` is an anamorphic
/// stretch (no aspect preservation) — identical to the sidecar and the
/// browser's `drawImage(..., 512, 512)`.
const PDQ_DIM: u32 = 512;

/// Exact RGB24 frame size: 512 * 512 * 3. A decode that yields fewer bytes
/// (timestamp past EOF on a short clip) is treated as a missing frame.
const PDQ_RGB_BYTES: usize = (PDQ_DIM as usize) * (PDQ_DIM as usize) * 3;

/// PDQ quality floor (0..=100). Mirrors the sidecar `DEFAULT_QUALITY_MIN = 50`;
/// blurry / low-information frames hash unreliably, so we drop them client-side
/// (the server keeps none below threshold anyway).
const PDQ_QUALITY_MIN: u8 = 50;

/// Compute the PDQ frame bag for a local video, off the async runtime.
///
/// Returns the frames in ascending-timestamp order (the server derives
/// `frame_index` from list position). Returns an empty vec when PDQ is disabled
/// or when nothing could be decoded/hashed — never an error, so a caller can
/// treat "no frames" and "PDQ off" identically and proceed with the upload.
pub async fn compute_pdq_frames(path: &Path) -> Vec<PdqFrameWire> {
    if !PDQ_ENABLED {
        return Vec::new();
    }
    let owned = path.to_path_buf();
    match tokio::task::spawn_blocking(move || compute_pdq_frames_blocking(&owned)).await {
        Ok(frames) => frames,
        Err(e) => {
            tracing::warn!("pdq: frame extraction task panicked: {e}");
            Vec::new()
        }
    }
}

/// Blocking core: resolve the bundled ffmpeg once, then extract+hash each
/// timestamp. Per-frame failures are dropped via `filter_map`; the result is
/// whatever subset hashed cleanly (possibly empty).
fn compute_pdq_frames_blocking(path: &Path) -> Vec<PdqFrameWire> {
    let ffmpeg = crate::desensitize::resolve_ffmpeg_binary();
    PDQ_TIMES
        .iter()
        .filter_map(|&t| hash_frame_at(&ffmpeg, path, t))
        .collect()
}

/// Decode one 512x512 rgb24 frame at `t` seconds and PDQ-hash it. Returns
/// `None` (skip this timestamp) when the frame can't be decoded, the read is
/// short (past EOF), the hash fails, or quality is below [`PDQ_QUALITY_MIN`].
fn hash_frame_at(ffmpeg: &OsStr, path: &Path, t: u32) -> Option<PdqFrameWire> {
    let rgb = extract_frame_rgb24(ffmpeg, path, t)?;
    let (hash, quality) = compute_pdq_hash(&rgb)?;
    if quality < PDQ_QUALITY_MIN {
        tracing::debug!(t, quality, "pdq: dropping below-quality frame");
        return None;
    }
    Some(PdqFrameWire {
        t: f64::from(t),
        hash,
        quality,
    })
}

/// Run the bundled ffmpeg to seek to `t` and emit exactly one 512x512 rgb24
/// frame as raw bytes on stdout. The argv is byte-identical to the server
/// sidecar (`scripts/pdq/http_server.py`) so the decoded pixels — and therefore
/// the PDQ hash — line up across client and server. `-ss` is placed BEFORE
/// `-i` (input seeking) to match the sidecar's keyframe-snap behaviour; moving
/// it after `-i` would pick a different frame and break parity.
fn extract_frame_rgb24(ffmpeg: &OsStr, path: &Path, t: u32) -> Option<Vec<u8>> {
    let secs = t.to_string();
    let scale = format!("scale={PDQ_DIM}:{PDQ_DIM}");
    let result = crate::desensitize::hidden_command(ffmpeg)
        .args(["-v", "error", "-ss", secs.as_str()])
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            scale.as_str(),
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .output();

    let Output {
        status, mut stdout, ..
    } = match result {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(t, "pdq: ffmpeg spawn failed: {e}");
            return None;
        }
    };

    if !status.success() || stdout.len() < PDQ_RGB_BYTES {
        tracing::debug!(
            t,
            bytes = stdout.len(),
            "pdq: no full frame decoded (EOF or decode miss)"
        );
        return None;
    }
    // ffmpeg emits exactly one frame, but guard against any trailing bytes so
    // the hashed buffer is always exactly w*h*3.
    stdout.truncate(PDQ_RGB_BYTES);
    Some(stdout)
}

/// PDQ-hash one 512x512 rgb24 buffer. Wraps the bytes as a `DynamicImage` (via
/// the `image` version `pdqhash` re-exports) and runs Meta PDQ over the full
/// resolution (no pre-downscale, matching the sidecar's 512x512 input), then
/// renders the 256-bit digest as 64 lowercase hex (`packbits big`, matching the
/// server-side `PdqHash`) and clamps quality to 0..=100. Returns `None` only if
/// the buffer can't be wrapped as an image (wrong length — already guarded by
/// the caller).
fn compute_pdq_hash(rgb: &[u8]) -> Option<(String, u8)> {
    use pdqhash::image::{DynamicImage, RgbImage};
    let img = RgbImage::from_raw(PDQ_DIM, PDQ_DIM, rgb.to_vec())?;
    let (hash, quality) = pdqhash::generate_pdq_full_size(&DynamicImage::ImageRgb8(img));
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    // The `pdqhash` crate returns quality as a 0..=1 fraction (gradient_sum/90,
    // clamped to 1.0). Meta's reference, the Python sidecar (`quality_min=50`),
    // the wire `PdqFrameIn.quality: Int`, and our [`PDQ_QUALITY_MIN`] are all on
    // the 0..=100 scale — so scale up by 100, else every real frame rounds to
    // 0/1 and the quality gate drops the whole bag (no frames ever sent).
    let quality = (quality * 100.0).round().clamp(0.0, 100.0) as u8;
    Some((hex, quality))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The on-wire hash is exactly 64 lowercase hex chars (`PdqHash` server-side
    /// validates `^[0-9a-fA-F]{64}$`). A grey 512x512 frame is enough to pin the
    /// shape regardless of the hash bits.
    #[test]
    fn pdq_hash_is_64_lowercase_hex() {
        let rgb = vec![128u8; PDQ_RGB_BYTES];
        let (hash, quality) = compute_pdq_hash(&rgb).expect("grey frame hashes");
        assert_eq!(hash.len(), 64, "PDQ hash must be 64 hex chars");
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash must be lowercase hex, got: {hash}"
        );
        assert!(quality <= 100, "quality clamped to 0..=100");
    }

    /// BIT-EXACTNESS GATE (pending a dev environment). PDQ near-dup only works
    /// if a desktop-computed hash equals the sidecar/browser hash for the same
    /// 512x512 rgb24 frame BYTE-FOR-BYTE (distance 0, not merely <= the D=31
    /// match budget). Wire the 159 reference vectors / a captured sidecar
    /// fixture here and assert equality before flipping `PDQ_ENABLED` to true.
    #[test]
    #[ignore = "pending 159 reference vectors / sidecar fixtures (see PDQ_ENABLED)"]
    fn pdq_hash_bit_exact_with_sidecar() {
        // for (rgb, expected_hex) in REFERENCE_VECTORS { assert_eq!(compute_pdq_hash(&rgb).unwrap().0, expected_hex); }
    }
}
