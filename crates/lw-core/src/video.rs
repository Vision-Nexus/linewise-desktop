use crate::config::TranscodeConfig;
use crate::error::VideoValidationError;
use crate::models::{Acceptance, VideoInfo, VideoValidationResult};
use std::path::Path;

/// Expected video parameters with tolerance ranges (advisory, not blocking)
const FPS_MIN: f64 = 20.0;
const FPS_MAX: f64 = 40.0;
const FPS_TARGET: f64 = 30.0;
const RESOLUTION_MIN_HEIGHT: u32 = 720;
const BITRATE_MIN_KBPS: u64 = 10_000;
const BITRATE_MAX_KBPS: u64 = 35_000;
const BITRATE_TARGET_KBPS: u64 = 30_000;

/// Hard acceptance floor — clips that fail any of these go to the REJECTED
/// state and never advance to upload. Distinct from the advisory ranges
/// above: those produce yellow warnings, these produce a red row the user
/// has to remove or re-shoot.
///
///   - bitrate at least 8 Mbps;
///   - fps in [20, 70];
///   - pixel count in [1920×1080, 3840×2160]. Inclusive on both ends, so
///     canonical 1080p (and the portrait-orientation 1080×1920 form, which
///     has the same pixel count) passes; canonical 4K UHD also passes.
///     Anything below 1080p or above 4K UHD is rejected.
const ACCEPT_BITRATE_MIN_KBPS: u64 = 8_000;
const ACCEPT_FPS_MIN: f64 = 20.0;
const ACCEPT_FPS_MAX: f64 = 70.0;
const ACCEPT_PIXELS_MIN: u64 = 1920 * 1080;
const ACCEPT_PIXELS_MAX: u64 = 3840 * 2160;

/// Link to guide users on how to change camera settings
pub const CAMERA_SETTINGS_GUIDE: &str = "https://docs.linewise.io/camera-settings";

/// Container / stream metadata keys whose presence is the strongest evidence
/// the file is camera-original. iPhones and most action cameras write at
/// least one of these; a third-party transcode (HandBrake, ffmpeg, NLE
/// export) drops them. Match is case-insensitive on the key.
const CAMERA_FINGERPRINT_KEYS: &[&str] = &[
    "make",
    "model",
    "com.apple.quicktime.make",
    "com.apple.quicktime.model",
    "com.android.manufacturer",
    "com.android.model",
];

/// Metadata keys that hold the encoder / authoring-tool name. We read these
/// to classify *why* the camera fingerprint is missing — was it stripped by
/// a known re-encode tool, or just absent for unknown reasons?
const ENCODER_KEYS: &[&str] = &["encoder", "com.apple.quicktime.software"];

/// Substrings (case-insensitive, leading whitespace trimmed) that, when seen
/// in an encoder tag, identify a re-encode pass by a non-camera tool.
const REENCODE_SIGNATURES: &[&str] = &[
    "lavf",
    "lavc",
    "handbrake",
    "apple compressor",
    "compressor",
    "adobe premiere",
    "final cut",
    "davinci resolve",
    "ffmpeg",
    "libx264",
    "libx265",
];

/// Substrings that, when seen in an encoder tag, identify a *camera* (not a
/// re-encode tool). DJI / GoPro / Insta360 / Sony etc. write their model
/// name into the `encoder` field rather than into `make`/`model`, so an
/// "encoder = DJI Osmo Nano" file is camera-original even though it has no
/// `make` key. Match is case-insensitive after trimming.
///
/// `(needle, vendor_label)` — the second element is the canonical vendor
/// name we surface in the device popover row when the encoder string is
/// the only fingerprint we have. Keep it in sync with what cameras
/// actually write; this is a recognition list, not an enumeration.
const DEVICE_ENCODER_SIGNATURES: &[(&str, &str)] = &[
    ("dji", "DJI"),
    ("gopro", "GoPro"),
    ("insta360", "Insta360"),
    ("sony", "Sony"),
    ("canon", "Canon"),
    ("nikon", "Nikon"),
    ("panasonic", "Panasonic"),
    ("blackmagic", "Blackmagic Design"),
    ("hero", "GoPro"),
    ("osmo", "DJI"),
];

/// Container codec_tag values we recognize as per-frame telemetry streams.
/// FFmpeg sees them as `media::Type::Data` with a four-CC tag; we don't
/// decode the binary payload, only detect that the stream exists.
const TELEMETRY_TAGS: &[(&[u8; 4], &str)] = &[
    (b"djmd", "DJI CAM metadata"),
    (b"dbgi", "DJI debug info"),
    (b"gpmd", "GoPro telemetry (GPMF)"),
    (b"mett", "ISO timed metadata"),
];

/// Verdict on where a clip came from. Drives the metadata-loss warning.
/// Private to this module — only the resulting warning string crosses the
/// boundary.
#[derive(Debug, PartialEq, Eq)]
enum Provenance {
    /// At least one camera-fingerprint key is present. No warning.
    CameraOriginal,
    /// No camera fingerprint, but the encoder tag matches a known re-encode
    /// signature. The captured `tool` string is shown to the user.
    Reencoded { tool: String },
    /// No camera fingerprint and no recognizable encoder tag. We can't say
    /// what touched the file, only that the device fingerprint is gone.
    Stripped,
}

fn classify_provenance(ictx: &ffmpeg_next::format::context::Input) -> Provenance {
    let container = ictx.metadata();
    // Stream metadata can carry encoder info on a per-stream level (most
    // common for MOV's `com.apple.quicktime.software`), so eagerly copy
    // any matching key into an owned String. We can't return a `&str`
    // borrowed from the stream's DictionaryRef because the underlying
    // Stream wrapper goes out of scope at the end of `.map(...)`.
    let stream_lookup = |keys: &[&str]| -> Option<String> {
        let stream = ictx.streams().best(ffmpeg_next::media::Type::Video)?;
        let dict = stream.metadata();
        keys.iter().find_map(|k| dict.get(k).map(str::to_owned))
    };
    let any_present = |keys: &[&str]| -> bool {
        keys.iter().any(|k| container.get(k).is_some()) || stream_lookup(keys).is_some()
    };
    let first_value = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| container.get(k).map(str::to_owned))
            .or_else(|| stream_lookup(keys))
    };

    let camera_present = any_present(CAMERA_FINGERPRINT_KEYS);
    let encoder_tag = first_value(ENCODER_KEYS);
    classify_from_signals(camera_present, encoder_tag.as_deref())
}

/// Pure decision helper. Splits the I/O (reading dict keys from an ffmpeg
/// Input) from the classification logic so the latter is unit-testable
/// without spinning up a real format context.
///
/// Order of evidence: an explicit camera-fingerprint key (`make`, `model`,
/// the Apple/Android variants) wins outright. Otherwise we look at the
/// encoder tag — a known re-encode signature flags `Reencoded`, a known
/// camera vendor still flags `CameraOriginal` (DJI / GoPro / Insta360 etc.
/// all write their model into `encoder` rather than into `make`). Only
/// when neither pattern matches do we fall through to `Stripped`.
fn classify_from_signals(camera_present: bool, encoder_tag: Option<&str>) -> Provenance {
    if camera_present {
        return Provenance::CameraOriginal;
    }
    let Some(tool) = encoder_tag else {
        return Provenance::Stripped;
    };
    let needle = tool.trim().to_ascii_lowercase();
    if REENCODE_SIGNATURES.iter().any(|sig| needle.contains(sig)) {
        Provenance::Reencoded {
            tool: tool.trim().to_string(),
        }
    } else if DEVICE_ENCODER_SIGNATURES
        .iter()
        .any(|(sig, _)| needle.contains(sig))
    {
        Provenance::CameraOriginal
    } else {
        // An encoder tag we don't recognize. Treat as Stripped — we don't
        // have enough signal to claim "re-encoded by X", but the camera
        // fingerprint is gone all the same.
        Provenance::Stripped
    }
}

/// Decide whether a probed clip clears the acceptance floor.
/// Three structural rules (bitrate / fps / resolution) plus a provenance
/// rule: no recognizable device fingerprint means the file was almost
/// certainly re-encoded by another tool, which we refuse to ingest because
/// the original camera evidence is gone.
fn classify_acceptance(info: &VideoInfo, provenance: &Provenance) -> Acceptance {
    let mut reasons: Vec<String> = Vec::new();

    // Bitrate floor. `0` means we couldn't read a bitrate at all — don't
    // block on missing data; the warning band already mentions it.
    if info.bitrate_kbps > 0 && info.bitrate_kbps < ACCEPT_BITRATE_MIN_KBPS {
        reasons.push(format!(
            "Bitrate {}kbps is below the {}Mbps acceptance floor",
            info.bitrate_kbps,
            ACCEPT_BITRATE_MIN_KBPS / 1000,
        ));
    }

    // Fps must be strictly inside the (min, max) band as written.
    // `fps == 0.0` again means "couldn't read" — skip rather than block.
    if info.fps > 0.0 && (info.fps < ACCEPT_FPS_MIN || info.fps > ACCEPT_FPS_MAX) {
        reasons.push(format!(
            "Frame rate {:.1}fps is outside the {}–{}fps acceptance band",
            info.fps, ACCEPT_FPS_MIN as u32, ACCEPT_FPS_MAX as u32,
        ));
    }

    // Pixel count: in [1080p, 4K UHD]. Inclusive on both ends so 1920×1080
    // and 3840×2160 (and their portrait twins) all pass.
    let pixels = u64::from(info.width) * u64::from(info.height);
    if pixels > 0 && !(ACCEPT_PIXELS_MIN..=ACCEPT_PIXELS_MAX).contains(&pixels) {
        reasons.push(format!(
            "Resolution {}x{} is outside the 1080p–4K acceptance band",
            info.width, info.height,
        ));
    }

    // Provenance: anything that isn't camera-original is rejected.
    // `Reencoded` is the explicit positive identification; `Stripped` is
    // "we can't prove camera-original and the encoder tag is missing /
    // unrecognized" — both fail the gate.
    match provenance {
        Provenance::CameraOriginal => {}
        Provenance::Reencoded { tool } => {
            reasons.push(format!(
                "Source was re-encoded by {tool}; original camera fingerprint is gone"
            ));
        }
        Provenance::Stripped => {
            reasons.push(
                "Device metadata is missing — the file was likely re-encoded by another tool"
                    .to_string(),
            );
        }
    }

    if reasons.is_empty() {
        Acceptance::Accepted
    } else {
        Acceptance::Rejected { reasons }
    }
}

/// Normalize the device-info group into labelled rows. Multiple raw keys
/// fold into a single line each: `make` and any `*.make` variant become
/// "Make"; same for `model` and `software`. Returns an empty string in
/// the value when nothing was found, so the UI can render the absence
/// instead of hiding the row — that's the whole point of showing device
/// info on REJECTED clips.
///
/// When the file has no `make`/`model` keys but the encoder string names
/// a known camera vendor (DJI / GoPro / etc.), we split the encoder tag
/// into a vendor + remainder pair so the device row reads "DJI / Osmo
/// Nano" instead of leaving Make and Model empty.
pub fn device_info_rows(info: &VideoInfo) -> Vec<(&'static str, String)> {
    let lookup = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| {
            info.metadata
                .iter()
                .find_map(|(mk, mv)| (mk == k).then(|| mv.clone()))
        })
    };

    let make_keys = &[
        "make",
        "com.apple.quicktime.make",
        "com.android.manufacturer",
        "device_manufacturer",
    ];
    let model_keys = &[
        "model",
        "com.apple.quicktime.model",
        "com.android.model",
        "device_model",
    ];

    let mut make = lookup(make_keys).unwrap_or_default();
    let mut model = lookup(model_keys).unwrap_or_default();
    let software = lookup(&["com.apple.quicktime.software", "encoder"]).unwrap_or_default();
    let recorded = lookup(&["creation_time"]).unwrap_or_default();

    // DJI/GoPro/etc. fallback: if make/model are empty but the encoder
    // string names a vendor, split it. "DJI Osmo Nano" → make=DJI,
    // model=Osmo Nano. Pure encoder strings like "HEVC" don't match and
    // leave the rows empty.
    if make.is_empty() && model.is_empty() && !software.is_empty() {
        let needle = software.trim().to_ascii_lowercase();
        if let Some((_, vendor)) = DEVICE_ENCODER_SIGNATURES
            .iter()
            .find(|(sig, _)| needle.contains(sig))
        {
            make = (*vendor).to_string();
            // Strip vendor tokens from the encoder string; what's left is
            // the model. If nothing's left (encoder was just the vendor
            // name), fall back to the full encoder so the row shows
            // something.
            let remainder = software
                .split_whitespace()
                .filter(|w| !w.eq_ignore_ascii_case(vendor))
                .collect::<Vec<_>>()
                .join(" ");
            model = if remainder.is_empty() {
                software.clone()
            } else {
                remainder
            };
        }
    }

    vec![
        ("Make", make),
        ("Model", model),
        ("Software", software),
        ("Recorded", recorded),
    ]
}

/// Dump every readable container + per-stream tag into a flat list. Pairs
/// are deduped on `(scope, key)`, with the container scope listed first.
/// This is the "show all the text/numbers we have" surface the popover
/// renders — we don't curate which keys make it through.
fn collect_popover_metadata(ictx: &ffmpeg_next::format::context::Input) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (k, v) in ictx.metadata().iter() {
        if seen.insert(k.to_string()) {
            out.push((k.to_string(), v.to_string()));
        }
    }
    for stream in ictx.streams() {
        let prefix = match stream.parameters().medium() {
            ffmpeg_next::media::Type::Video => "video",
            ffmpeg_next::media::Type::Audio => "audio",
            ffmpeg_next::media::Type::Data => "data",
            ffmpeg_next::media::Type::Subtitle => "subtitle",
            ffmpeg_next::media::Type::Attachment => "attachment",
            ffmpeg_next::media::Type::Unknown => continue,
        };
        for (k, v) in stream.metadata().iter() {
            // Namespace per-stream keys so they don't collide with the
            // container scope. `creation_time` shows up at both levels;
            // the user wants to see both rather than one silently shadowing
            // the other.
            let key = format!("{prefix}/{k}");
            if seen.insert(key.clone()) {
                out.push((key, v.to_string()));
            }
        }
    }
    out
}

/// Walk the input streams looking for known telemetry data streams. A
/// match returns the human-readable label we surface in the popover; we
/// don't decode the binary payload, only detect that it's there.
pub fn detect_telemetry(info_streams: &[(ffmpeg_next::media::Type, [u8; 4])]) -> Option<String> {
    info_streams.iter().find_map(|(medium, tag)| match medium {
        ffmpeg_next::media::Type::Data => TELEMETRY_TAGS
            .iter()
            .find(|(needle, _)| needle == &tag)
            .map(|(_, label)| label.to_string()),
        _ => None,
    })
}

fn collect_data_streams(
    ictx: &ffmpeg_next::format::context::Input,
) -> Vec<(ffmpeg_next::media::Type, [u8; 4])> {
    ictx.streams()
        .map(|s| {
            let medium = s.parameters().medium();
            // SAFETY: codec_tag is a u32 little-endian four-CC stored on
            // codecpar; reading it is a plain field access. ffmpeg-next
            // doesn't expose it through a wrapper.
            let tag_u32 = unsafe { (*s.parameters().as_ptr()).codec_tag };
            (medium, tag_u32.to_le_bytes())
        })
        .collect()
}

/// Heuristic: does this clip *look like* it should carry telemetry? Used
/// to decide whether to warn when telemetry is missing — we don't want to
/// nag iPhone users about a missing GPMF stream.
fn looks_like_action_camera(info: &VideoInfo) -> bool {
    let has_action_keyword = |s: &str| {
        let n = s.to_ascii_lowercase();
        n.contains("dji")
            || n.contains("gopro")
            || n.contains("hero")
            || n.contains("osmo")
            || n.contains("insta360")
    };
    info.metadata.iter().any(|(_, v)| has_action_keyword(v))
}

fn provenance_warning(p: &Provenance) -> Option<String> {
    match p {
        Provenance::CameraOriginal => None,
        Provenance::Reencoded { tool } => Some(format!(
            "This clip was re-encoded by {tool}; the original camera fingerprint is gone."
        )),
        Provenance::Stripped => Some(
            "This clip's device metadata is missing — it may have been re-encoded or transcoded by another tool."
                .to_string(),
        ),
    }
}

/// Probe a video file using ffmpeg-next and validate against target parameters
pub async fn validate_video(path: &Path) -> Result<VideoValidationResult, VideoValidationError> {
    let path = path.to_path_buf();

    tokio::task::spawn_blocking(move || probe_and_validate(&path))
        .await
        .map_err(|e| VideoValidationError::ProbeFailed(e.to_string()))?
}

fn probe_and_validate(path: &Path) -> Result<VideoValidationResult, VideoValidationError> {
    // Treat the three structural failure points below as `Unplayable`. They
    // fire when the container has no usable timeline — most often a missing
    // `moov` atom from a power-cut MP4/MOV recording, but also truncated
    // headers and unreadable codec parameters. The libav error string is
    // already informative ("Invalid data found when processing input" for
    // the moov case), so we keep it in the reason field.
    let ictx = ffmpeg_next::format::input(path).map_err(|e| VideoValidationError::Unplayable {
        reason: format!(
            "container could not be opened (likely missing moov atom or truncated): {e}"
        ),
    })?;

    let video_stream = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or_else(|| VideoValidationError::Unplayable {
            reason: "no video stream in container".to_string(),
        })?;

    let video_params = video_stream.parameters();
    let video_ctx =
        ffmpeg_next::codec::context::Context::from_parameters(video_params).map_err(|e| {
            VideoValidationError::Unplayable {
                reason: format!("video stream codec parameters unreadable: {e}"),
            }
        })?;
    let video_dec = video_ctx
        .decoder()
        .video()
        .map_err(|e| VideoValidationError::Unplayable {
            reason: format!("video decoder could not be initialised: {e}"),
        })?;

    let width = video_dec.width();
    let height = video_dec.height();
    let codec = video_stream.parameters().id().name().to_string();

    // Frame rate from stream
    let rate = video_stream.rate();
    let fps = if rate.1 > 0 {
        rate.0 as f64 / rate.1 as f64
    } else {
        0.0
    };

    // Bitrate: prefer decoder bit_rate, fallback to format-level
    let bitrate_bps = if video_dec.bit_rate() > 0 {
        video_dec.bit_rate() as u64
    } else {
        ictx.bit_rate().max(0) as u64
    };
    let bitrate_kbps = bitrate_bps / 1000;

    // Duration from format context (in seconds)
    let duration_secs = ictx.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE);

    // Format name
    let format = ictx.format().name().to_string();

    // Find audio stream codec
    let audio_codec = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .map(|s| s.parameters().id().name().to_string())
        .unwrap_or_default();

    let metadata = collect_popover_metadata(&ictx);
    let telemetry = detect_telemetry(&collect_data_streams(&ictx));

    let info = VideoInfo {
        width,
        height,
        fps,
        bitrate_kbps,
        codec,
        audio_codec,
        duration_secs,
        format,
        metadata,
        telemetry,
    };

    let mut warnings = Vec::new();

    // FPS: acceptable range 20-40, target 30
    if fps > 0.0 && !(FPS_MIN..=FPS_MAX).contains(&fps) {
        warnings.push(format!(
            "Frame rate {fps:.1}fps is outside recommended range ({FPS_MIN:.0}-{FPS_MAX:.0}fps, target {FPS_TARGET:.0}fps)"
        ));
    }

    // Resolution: warn if below 720p
    if height > 0 && height < RESOLUTION_MIN_HEIGHT {
        warnings.push(format!(
            "Resolution {width}x{height} is below minimum recommended ({RESOLUTION_MIN_HEIGHT}p)"
        ));
    }

    // Bitrate: acceptable range 10M-35M
    if bitrate_kbps > 0 && !(BITRATE_MIN_KBPS..=BITRATE_MAX_KBPS).contains(&bitrate_kbps) {
        warnings.push(format!(
            "Bitrate {bitrate_kbps}kbps is outside recommended range ({}-{}Mbps, target {}Mbps)",
            BITRATE_MIN_KBPS / 1000,
            BITRATE_MAX_KBPS / 1000,
            BITRATE_TARGET_KBPS / 1000,
        ));
    }

    if !warnings.is_empty() {
        warnings.push(format!(
            "See how to adjust your camera settings: {CAMERA_SETTINGS_GUIDE}"
        ));
    }

    // Provenance check rides the same warnings vector but sits *after* the
    // camera-settings link footer, since the link doesn't apply — the user
    // can't fix re-encoding by changing a camera setting.
    let provenance = classify_provenance(&ictx);
    if let Some(msg) = provenance_warning(&provenance) {
        warnings.push(msg);
    }

    // Action-camera-shaped clip with no telemetry: warn but don't reject.
    // DJI / GoPro recordings normally carry a per-frame data stream
    // (djmd / gpmd) that downstream evidence-handling relies on; an
    // action-camera clip without one is usually the output of a re-encode
    // that stripped the data stream while preserving the encoder tag.
    if info.telemetry.is_none() && looks_like_action_camera(&info) {
        warnings.push(
            "This clip looks like action-camera footage but carries no telemetry stream — \
             gimbal / GPS / IMU samples will not be available downstream."
                .to_string(),
        );
    }

    let acceptance = classify_acceptance(&info, &provenance);

    Ok(VideoValidationResult {
        info,
        warnings,
        acceptance,
    })
}

/// Would transcoding actually shrink this clip? Returns false when the source
/// is at or below the target on resolution, fps, and bitrate — in that case
/// a transcode only costs CPU/storage without adding value. The UI uses this
/// to hide the per-clip transcode toggle; `upload::maybe_transcode` also
/// short-circuits on `false` as defense-in-depth.
pub fn transcode_would_help(info: &VideoInfo, cfg: &TranscodeConfig) -> bool {
    let resolution_exceeds = info.height > cfg.max_height;
    let fps_exceeds = cfg.target_fps > 0 && info.fps > cfg.target_fps as f64;
    let bitrate_exceeds = info.bitrate_kbps > (cfg.max_bitrate_mbps as u64) * 1000;
    resolution_exceeds || fps_exceeds || bitrate_exceeds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a minimal ISO-BMFF file containing only an `ftyp` box — no
    /// `moov`, no `mdat`. This is the shape of a power-cut MP4/MOV: the
    /// header arrives, but the trailing `moov` (which holds the index)
    /// never gets written. ffmpeg's `mov,mp4,m4a,3gp` demuxer rejects it
    /// with "moov atom not found / Invalid data found when processing
    /// input", which is exactly the case we want to detect.
    fn write_moovless_mp4(path: &std::path::Path) {
        // Box layout (big-endian):
        //   [size:u32][type:'ftyp'][major:'isom'][minor:0][compat:'isom','avc1']
        // Total: 24 bytes.
        let bytes: &[u8] = &[
            0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0x00, 0x00,
            0x02, 0x00, b'i', b's', b'o', b'm', b'a', b'v', b'c', b'1',
        ];
        std::fs::write(path, bytes).expect("write fixture");
    }

    #[tokio::test]
    async fn unplayable_when_moov_missing() {
        let path =
            std::env::temp_dir().join(format!("lw-test-no-moov-{}.mp4", uuid::Uuid::new_v4()));
        write_moovless_mp4(&path);

        let result = validate_video(&path).await;

        let _ = std::fs::remove_file(&path);

        match result {
            Err(VideoValidationError::Unplayable { reason }) => {
                assert!(
                    !reason.is_empty(),
                    "Unplayable reason should carry the libav error string, got empty"
                );
            }
            other => panic!("expected Unplayable, got {other:?}"),
        }
    }

    fn info(height: u32, fps: f64, bitrate_kbps: u64) -> VideoInfo {
        VideoInfo {
            width: 1920,
            height,
            fps,
            bitrate_kbps,
            codec: "h264".into(),
            audio_codec: "aac".into(),
            duration_secs: 60.0,
            format: "mov".into(),
            metadata: Vec::new(),
            telemetry: None,
        }
    }

    fn cfg() -> TranscodeConfig {
        TranscodeConfig {
            enabled: true,
            codec: "hevc".into(),
            crf: 23,
            preset: "medium".into(),
            target_bitrate_mbps: 10,
            max_bitrate_mbps: 20,
            max_height: 1080,
            audio_bitrate_kbps: 128,
            target_fps: 30,
            hw_accel: "auto".into(),
        }
    }

    #[test]
    fn guard_exceeds_resolution() {
        assert!(transcode_would_help(&info(1440, 30.0, 8_000), &cfg()));
    }

    #[test]
    fn guard_exceeds_fps() {
        assert!(transcode_would_help(&info(1080, 60.0, 8_000), &cfg()));
    }

    #[test]
    fn guard_exceeds_bitrate() {
        assert!(transcode_would_help(&info(1080, 30.0, 25_000), &cfg()));
    }

    #[test]
    fn guard_source_below_all() {
        // 720p 30fps 4Mbps — transcoding is pure waste.
        assert!(!transcode_would_help(&info(720, 30.0, 4_000), &cfg()));
    }

    #[test]
    fn guard_fps_ignored_when_target_zero() {
        let mut c = cfg();
        c.target_fps = 0;
        // 60fps at 720p 4Mbps with no target_fps → no axis exceeds.
        assert!(!transcode_would_help(&info(720, 60.0, 4_000), &c));
    }

    #[test]
    fn provenance_camera_original_when_make_present() {
        // iPhone-style: `make=Apple` is present alongside an encoder tag
        // that would otherwise look like a re-encode (Apple writes
        // `com.apple.quicktime.software`). Camera fingerprint wins.
        let v = classify_from_signals(true, Some("HandBrake 1.6.1"));
        assert_eq!(v, Provenance::CameraOriginal);
    }

    #[test]
    fn provenance_reencoded_when_known_signature() {
        let v = classify_from_signals(false, Some("Lavf60.16.100"));
        match v {
            Provenance::Reencoded { tool } => assert_eq!(tool, "Lavf60.16.100"),
            other => panic!("expected Reencoded, got {other:?}"),
        }
    }

    #[test]
    fn provenance_reencoded_case_insensitive_and_trimmed() {
        let v = classify_from_signals(false, Some("  HandBrake 1.6.1 "));
        match v {
            Provenance::Reencoded { tool } => assert_eq!(tool, "HandBrake 1.6.1"),
            other => panic!("expected Reencoded, got {other:?}"),
        }
    }

    #[test]
    fn provenance_stripped_when_no_metadata() {
        let v = classify_from_signals(false, None);
        assert_eq!(v, Provenance::Stripped);
    }

    #[test]
    fn provenance_stripped_when_encoder_unknown() {
        // An encoder string we don't recognize. We can't claim
        // "re-encoded by X", but the camera fingerprint is still gone.
        let v = classify_from_signals(false, Some("MysteryToolPro 9.9"));
        assert_eq!(v, Provenance::Stripped);
    }

    fn full_info(width: u32, height: u32, fps: f64, bitrate_kbps: u64) -> VideoInfo {
        VideoInfo {
            width,
            height,
            fps,
            bitrate_kbps,
            codec: "h264".into(),
            audio_codec: "aac".into(),
            duration_secs: 60.0,
            format: "mp4".into(),
            metadata: Vec::new(),
            telemetry: None,
        }
    }

    #[test]
    fn acceptance_clean_1440p_30fps_15mbps_passes() {
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_blocks_below_bitrate_floor() {
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 6_000),
            &Provenance::CameraOriginal,
        );
        match v {
            Acceptance::Rejected { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("Bitrate")), "{reasons:?}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn acceptance_blocks_below_fps_floor() {
        let v = classify_acceptance(
            &full_info(2560, 1440, 18.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_blocks_above_fps_ceiling() {
        let v = classify_acceptance(
            &full_info(2560, 1440, 120.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_accepts_exactly_1080p() {
        // 1920×1080 lies on the lower bound and is accepted (inclusive).
        let v = classify_acceptance(
            &full_info(1920, 1080, 30.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_accepts_portrait_1080p() {
        // 1080×1920 has the same pixel count as 1920×1080, so the
        // pixel-count rule treats it identically. Rotation lives in
        // side-data, not in the dimensions stored on the stream.
        let v = classify_acceptance(
            &full_info(1080, 1920, 30.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_accepts_exactly_4k() {
        // 3840×2160 lies on the upper bound and is accepted (inclusive).
        let v = classify_acceptance(
            &full_info(3840, 2160, 30.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_rejects_below_1080p() {
        let v = classify_acceptance(
            &full_info(1280, 720, 30.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_rejects_above_4k() {
        // 8K, 7680×4320 — well past the 4K ceiling.
        let v = classify_acceptance(
            &full_info(7680, 4320, 30.0, 15_000),
            &Provenance::CameraOriginal,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_skips_when_unreadable() {
        // bitrate=0, fps=0, dimensions=0 — probe couldn't read structural
        // fields. We don't block on missing data; the unplayable path
        // already errored out earlier if the file is truly broken.
        let v = classify_acceptance(&full_info(0, 0, 0.0, 0), &Provenance::CameraOriginal);
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_blocks_reencoded_clip() {
        // Otherwise-clean clip, but the source was re-encoded — reject.
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 15_000),
            &Provenance::Reencoded {
                tool: "HandBrake 1.6.1".into(),
            },
        );
        match v {
            Acceptance::Rejected { reasons } => {
                assert!(
                    reasons.iter().any(|r| r.contains("HandBrake")),
                    "{reasons:?}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn acceptance_blocks_stripped_clip() {
        let v = classify_acceptance(&full_info(2560, 1440, 30.0, 15_000), &Provenance::Stripped);
        match v {
            Acceptance::Rejected { reasons } => {
                assert!(
                    reasons.iter().any(|r| r.contains("Device metadata")),
                    "{reasons:?}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn provenance_dji_encoder_is_camera_original() {
        // The exact case the user reported: encoder=DJI Osmo Nano with no
        // make/model keys. Should classify as camera-original.
        let v = classify_from_signals(false, Some("DJI Osmo Nano"));
        assert_eq!(v, Provenance::CameraOriginal);
    }

    #[test]
    fn provenance_gopro_encoder_is_camera_original() {
        let v = classify_from_signals(false, Some("GoPro HERO12"));
        assert_eq!(v, Provenance::CameraOriginal);
    }

    #[test]
    fn device_info_splits_dji_encoder() {
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata
            .push(("encoder".into(), "DJI Osmo Nano".into()));
        let rows = device_info_rows(&info);
        let make = rows.iter().find(|(k, _)| *k == "Make").unwrap().1.clone();
        let model = rows.iter().find(|(k, _)| *k == "Model").unwrap().1.clone();
        assert_eq!(make, "DJI");
        assert_eq!(model, "Osmo Nano");
    }

    #[test]
    fn device_info_keeps_existing_make_when_present() {
        // When `make` and `model` are present, the encoder split must not
        // overwrite them — the explicit camera tags win.
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata
            .push(("com.apple.quicktime.make".into(), "Apple".into()));
        info.metadata
            .push(("com.apple.quicktime.model".into(), "iPhone 13 Pro".into()));
        info.metadata
            .push(("com.apple.quicktime.software".into(), "15.0.1".into()));
        let rows = device_info_rows(&info);
        let make = rows.iter().find(|(k, _)| *k == "Make").unwrap().1.clone();
        let model = rows.iter().find(|(k, _)| *k == "Model").unwrap().1.clone();
        assert_eq!(make, "Apple");
        assert_eq!(model, "iPhone 13 Pro");
    }

    #[test]
    fn provenance_warning_message_shape() {
        assert!(provenance_warning(&Provenance::CameraOriginal).is_none());

        let msg = provenance_warning(&Provenance::Reencoded {
            tool: "HandBrake 1.6.1".into(),
        })
        .expect("Reencoded must produce a warning");
        assert!(msg.contains("re-encoded"));
        assert!(msg.contains("HandBrake 1.6.1"));

        let msg =
            provenance_warning(&Provenance::Stripped).expect("Stripped must produce a warning");
        assert!(msg.contains("device metadata is missing"));
    }
}
