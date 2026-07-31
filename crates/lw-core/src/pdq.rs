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
//!   - decode each via the bundled ffmpeg CLI with the SAME video filter
//!     ([`PDQ_VF_FILTER`]) — the single source of truth the sidecar must copy
//!     verbatim,
//!   - hash with Meta PDQ and render the 256-bit digest as 64 lowercase hex.
//!
//! ## Bit-exactness is load-bearing
//! The server stores each hash as `bit(256)` and matches via raw Hamming
//! distance. A hash that differs from the browser/sidecar hash space — wrong
//! bit order, a different PDQ variant, a different ffmpeg resize kernel — never
//! matches, silently. [`PDQ_ENABLED`] was flipped to `true` after the desktop
//! hash was verified to land in the server hash space end-to-end against a live
//! backend.
//!
//! A bare `scale=512:512` is NOT reproducible across ffmpeg versions, so it is
//! not enough that both producers "scale to 512":
//!   1. the swscale resize **kernel** is a compiled-in default that has changed
//!      between ffmpeg releases, and
//!   2. the yuv→rgb chroma interpolation + output range are inferred defaults.
//! Two desktop users on different OSes (each OS bundles a different ffmpeg
//! version today — see the release/test workflows and the xtask bundler) could
//! therefore hash the SAME source file to DIFFERENT bits. [`PDQ_VF_FILTER`]
//! pins the kernel and the convert step explicitly so the rgb24 bytes are a
//! function of the input file alone, not the ffmpeg minor version. ⚠️ Changing
//! this filter changes the produced hashes — it must roll out in lockstep with
//! the sidecar plus a server-side backfill of stored hashes.
//!
//! The `pdq_hash_self_consistency_reference` test is the enforced regression
//! gate: it hashes a deterministically-generated rgb24 frame and asserts the
//! 256-bit digest equals a committed reference vector, so any drift in the PDQ
//! core / hex rendering / quality scaling breaks the build. A second,
//! still-`#[ignore]`d slot (`pdq_hash_bit_exact_with_sidecar`) is reserved for
//! cross-producer reference vectors captured from the server sidecar
//! (`linewise-deploy/scripts/pdq`); wiring those closes the desktop↔sidecar
//! parity loop.
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

/// Square frame edge fed to PDQ. The `scale=512:512` in [`PDQ_VF_FILTER`] is an
/// anamorphic stretch (no aspect preservation) — identical to the sidecar and
/// the browser's `drawImage(..., 512, 512)`.
const PDQ_DIM: u32 = 512;

/// The ffmpeg `-vf` filter string — the SINGLE SOURCE OF TRUTH for how a source
/// frame is turned into the 512x512 rgb24 buffer PDQ hashes. The server sidecar
/// (`linewise-deploy/scripts/pdq/http_server.py`) and the browser path MUST use
/// this exact string so all producers land in the same hash space.
///
/// Every component is pinned on purpose, because a bare `scale=512:512` leaves
/// these to ffmpeg's compiled-in defaults (which drift across ffmpeg versions):
///   - `flags=bicubic` — pins the resize kernel. The default kernel has changed
///     between ffmpeg releases; `bicubic` is stable and matches the browser's
///     `drawImage` quality tier closely.
///   - `accurate_rnd` — deterministic rounding in the scaler (drops the fast,
///     version-dependent approximate-rounding path).
///   - `full_chroma_int+full_chroma_inp` — full (not fast/approximate) chroma
///     interpolation on both input and output, so the yuv→rgb upsampling is
///     identical regardless of build-time SIMD/codepath selection.
///   - `out_range=full` — emit full-range (0..=255) rgb. PDQ expects full-range
///     luma; without this the 8-bit levels shift by the tv/full delta.
///   - trailing `format=rgb24` — do the pixel-format convert inside the
///     filtergraph (deterministic) rather than relying on the encoder-side
///     `-pix_fmt` insertion point.
///
/// Note we deliberately do NOT force `in_color_matrix`/`in_range`: swscale reads
/// those from the source file's signalled colorspace, which is itself a property
/// of the input bytes — forcing bt709 would *mis*-convert correctly-tagged
/// bt601 footage. Determinism comes from the source-bytes → filter mapping being
/// fixed; both producers running this identical string is what makes it
/// bit-exact.
///
/// ⚠️ Changing this string changes every produced hash. It is a coordinated,
/// cross-repo change: ship it together with the sidecar update and a backfill
/// of already-stored hashes, or near-dup matching silently degrades.
const PDQ_VF_FILTER: &str = "scale=512:512:flags=bicubic+accurate_rnd+full_chroma_int+full_chroma_inp:out_range=full,format=rgb24";

/// Compile-time guard: the literal dimensions baked into [`PDQ_VF_FILTER`] must
/// match [`PDQ_DIM`]. The filter string can't interpolate a const, so if
/// `PDQ_DIM` ever changes this assertion forces the filter to be updated too.
const _: () = assert!(PDQ_DIM == 512, "PDQ_VF_FILTER hardcodes scale=512:512");

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
    let ffmpeg = crate::ffmpeg_util::resolve_ffmpeg_binary();
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
/// frame as raw bytes on stdout via the pinned [`PDQ_VF_FILTER`]. The argv is
/// byte-identical to the server sidecar (`scripts/pdq/http_server.py`) so the
/// decoded pixels — and therefore the PDQ hash — line up across client and
/// server. `-ss` is placed BEFORE `-i` (input seeking) to match the sidecar's
/// keyframe-snap behaviour; moving it after `-i` would pick a different frame
/// and break parity. `-pix_fmt rgb24` is kept as a belt-and-braces output
/// guarantee even though [`PDQ_VF_FILTER`] already ends in `format=rgb24`.
fn extract_frame_rgb24(ffmpeg: &OsStr, path: &Path, t: u32) -> Option<Vec<u8>> {
    let secs = t.to_string();
    let result = crate::ffmpeg_util::hidden_command(ffmpeg)
        .args(["-v", "error", "-ss", secs.as_str()])
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            PDQ_VF_FILTER,
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

    /// A deterministic, dependency-free 512x512 rgb24 frame whose pixels are a
    /// pure function of (x, y). Two callers — and two machines — that run this
    /// generator get byte-identical input, so [`compute_pdq_hash`] over it is a
    /// stable reference for the PDQ core + hex rendering + quality scaling.
    /// `kind` selects the spatial-frequency content so we can cover both the
    /// quality-saturating and the mid-quality paths.
    fn deterministic_rgb24(kind: ReferenceFrame) -> Vec<u8> {
        let mut buf = vec![0u8; PDQ_RGB_BYTES];
        for y in 0..PDQ_DIM {
            for x in 0..PDQ_DIM {
                let idx = ((y * PDQ_DIM + x) * 3) as usize;
                let (r, g, b) = match kind {
                    // High-frequency XOR interference — lots of edges, so PDQ's
                    // gradient-energy quality saturates at 100.
                    ReferenceFrame::HighFreq => (
                        (x ^ y) as u8,
                        (x.wrapping_mul(3) ^ y.wrapping_mul(5)) as u8,
                        x.wrapping_add(y) as u8,
                    ),
                    // Smooth horizontal grey ramp — low gradient energy, so the
                    // quality lands mid-scale (exercises the 0..1 → 0..=100
                    // scaling, not just the clamp).
                    ReferenceFrame::Smooth => {
                        let v = (x / 2) as u8;
                        (v, v, v)
                    }
                };
                buf[idx] = r;
                buf[idx + 1] = g;
                buf[idx + 2] = b;
            }
        }
        buf
    }

    #[derive(Clone, Copy)]
    enum ReferenceFrame {
        HighFreq,
        Smooth,
    }

    /// ENFORCED DETERMINISM GATE. Hash two checked-in deterministic frames and
    /// assert both the 256-bit digest AND the scaled quality match committed
    /// reference vectors. This breaks the build on ANY drift in the PDQ core
    /// (the `pdqhash` crate), the big-endian hex rendering, or the quality
    /// scaling — the three things that, together with [`PDQ_VF_FILTER`], define
    /// the desktop hash space.
    ///
    /// Reference vectors were computed with `pdqhash` 0.1.1 (the version pinned
    /// in the workspace `Cargo.toml`). A bump that changes the algorithm will
    /// fail here on purpose: regenerate the vectors only after confirming the
    /// server side moved in lockstep.
    #[test]
    fn pdq_hash_self_consistency_reference() {
        let cases = [
            (
                ReferenceFrame::HighFreq,
                "2aaa32aa2f773aaf505072fa2f7550dd5d00732a2f55110df7f272322d557232",
                100u8,
            ),
            (
                ReferenceFrame::Smooth,
                "aa2a3d5f2a2a55b55900b557a8024c9a5b48cd37b2a617dfa5aa9e8d166caeb3",
                70u8,
            ),
        ];
        for (kind, expected_hex, expected_quality) in cases {
            let rgb = deterministic_rgb24(kind);
            let (hash, quality) = compute_pdq_hash(&rgb).expect("deterministic frame hashes");
            assert_eq!(
                hash, expected_hex,
                "PDQ digest drifted — pdqhash core or hex rendering changed"
            );
            assert_eq!(
                quality, expected_quality,
                "PDQ quality scaling drifted from the reference"
            );
        }
    }

    /// CROSS-PRODUCER BIT-EXACTNESS GATE (fixtures pending). Self-consistency
    /// above pins the desktop hash to its own past. This gate pins it to the
    /// OTHER producers: the desktop hash must equal the server sidecar hash for
    /// the SAME source frame, BYTE-FOR-BYTE (Hamming distance 0, not merely
    /// within the D=31 match budget). It stays `#[ignore]`d until the fixtures
    /// land.
    ///
    /// To populate: capture the rgb24 frame the sidecar feeds Meta PDQ for a
    /// known input (run `linewise-deploy/scripts/pdq/http_server.py` with the
    /// pinned ffmpeg + `PDQ_VF_FILTER`, dump the post-filter rawvideo and the
    /// sidecar's 64-hex output), check the rgb24 frame in under `tests/fixtures/`,
    /// load it here, and `assert_eq!(compute_pdq_hash(&rgb).unwrap().0, expected)`.
    /// Then remove the `#[ignore]`.
    #[test]
    #[ignore = "pending sidecar reference rgb24 frame + 64-hex from linewise-deploy/scripts/pdq"]
    fn pdq_hash_bit_exact_with_sidecar() {
        // let rgb = std::fs::read("tests/fixtures/pdq_sidecar_frame.rgb24").unwrap();
        // const SIDECAR_HEX: &str = "<paste the sidecar's 64-hex output here>";
        // assert_eq!(compute_pdq_hash(&rgb).expect("fixture hashes").0, SIDECAR_HEX);
    }
}
