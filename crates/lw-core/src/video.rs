use crate::config::TranscodeConfig;
use crate::error::VideoValidationError;
use crate::models::{Acceptance, VideoInfo, VideoValidationResult};
use crate::video_rules::{
    Band, DeviceEncoderSignature, ProvenanceRules, Sub, TelemetryRules, VideoRules, render,
};
use std::path::Path;
use std::sync::Arc;

/// Render a bitrate as a short, human-readable string. ≥1000 kbps becomes
/// `54.3Mbps` (one decimal); below 1000 stays in `kbps`. Used for both the
/// popover summary line and the validation warnings, so the units the user
/// sees match across surfaces.
pub fn format_bitrate(kbps: u64) -> String {
    if kbps >= 1000 {
        format!("{:.1}Mbps", kbps as f64 / 1000.0)
    } else {
        format!("{kbps}kbps")
    }
}

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

fn classify_provenance(
    ictx: &ffmpeg_next::format::context::Input,
    rules: &ProvenanceRules,
) -> Provenance {
    let container = ictx.metadata();
    // Stream metadata can carry encoder info on a per-stream level (most
    // common for MOV's `com.apple.quicktime.software`), so eagerly copy
    // any matching key into an owned String. We can't return a `&str`
    // borrowed from the stream's DictionaryRef because the underlying
    // Stream wrapper goes out of scope at the end of `.map(...)`.
    let stream_lookup = |keys: &[String]| -> Option<String> {
        let stream = ictx.streams().best(ffmpeg_next::media::Type::Video)?;
        let dict = stream.metadata();
        keys.iter().find_map(|k| dict.get(k).map(str::to_owned))
    };
    let any_present = |keys: &[String]| -> bool {
        keys.iter().any(|k| container.get(k).is_some()) || stream_lookup(keys).is_some()
    };
    let first_value = |keys: &[String]| -> Option<String> {
        keys.iter()
            .find_map(|k| container.get(k).map(str::to_owned))
            .or_else(|| stream_lookup(keys))
    };

    let camera_present = any_present(&rules.camera_fingerprint_keys);
    let encoder_tag = first_value(&rules.encoder_keys);
    classify_from_signals(camera_present, encoder_tag.as_deref(), rules)
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
fn classify_from_signals(
    camera_present: bool,
    encoder_tag: Option<&str>,
    rules: &ProvenanceRules,
) -> Provenance {
    if camera_present {
        return Provenance::CameraOriginal;
    }
    let Some(tool) = encoder_tag else {
        return Provenance::Stripped;
    };
    let needle = tool.trim().to_ascii_lowercase();
    if rules
        .reencode_signatures
        .iter()
        .any(|sig| needle.contains(sig.as_str()))
    {
        Provenance::Reencoded {
            tool: tool.trim().to_string(),
        }
    } else if rules
        .device_encoder_signatures
        .iter()
        .any(|sig| needle.contains(sig.needle.as_str()))
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
fn classify_acceptance(
    info: &VideoInfo,
    provenance: &Provenance,
    rules: &VideoRules,
) -> Acceptance {
    let mut reasons: Vec<String> = Vec::new();

    // Bitrate. `0` means we couldn't read a bitrate at all — don't block
    // on missing data; the warning band already mentions it.
    if info.bitrate_kbps > 0
        && let Some(side) = trip(&rules.numeric.bitrate_kbps.accept, info.bitrate_kbps)
    {
        reasons.push(render_bitrate_band(
            &rules.numeric.bitrate_kbps.accept,
            rules.numeric.bitrate_kbps.target,
            info.bitrate_kbps,
            side,
        ));
    }

    // Fps. `0.0` again means "couldn't read" — skip rather than block.
    if info.fps > 0.0
        && let Some(side) = trip(&rules.numeric.fps.accept, info.fps)
    {
        reasons.push(render_fps_band(
            &rules.numeric.fps.accept,
            rules.numeric.fps.target,
            info.fps,
            side,
        ));
    }

    // Duration. `0.0` means we couldn't read a length — don't block.
    if info.duration_secs > 0.0
        && let Some(side) = trip(&rules.numeric.duration_seconds.accept, info.duration_secs)
    {
        reasons.push(render_duration_band(
            &rules.numeric.duration_seconds.accept,
            rules.numeric.duration_seconds.target,
            info.duration_secs,
            side,
        ));
    }

    // Pixel count: same band semantics as fps and bitrate. Both edges
    // inclusive — equal-on-edge passes — so a clip exactly at 720p
    // (the floor) is still acceptable.
    let pixels = u64::from(info.width) * u64::from(info.height);
    if pixels > 0
        && let Some(side) = trip(&rules.numeric.resolution.accept, pixels)
    {
        reasons.push(render_resolution_band(
            &rules.numeric.resolution.accept,
            info.width,
            info.height,
            side,
        ));
    }

    // Provenance: anything that isn't camera-original is rejected.
    // `Reencoded` is the explicit positive identification; `Stripped` is
    // "we can't prove camera-original and the encoder tag is missing /
    // unrecognized" — both fail the gate.
    match provenance {
        Provenance::CameraOriginal => {}
        Provenance::Reencoded { tool } => {
            reasons.push(render(
                &rules.provenance.messages.reencoded,
                &[("tool", Sub::Str(tool.clone()))],
            ));
        }
        Provenance::Stripped => {
            reasons.push(rules.provenance.messages.stripped.clone());
        }
    }

    if reasons.is_empty() {
        Acceptance::Accepted
    } else {
        Acceptance::Rejected { reasons }
    }
}

/// Which side of a band a numeric value tripped — `Below` means it fell
/// past `min`, `Above` means it exceeded `max`. The `as_str` rendering
/// is what `{bound}` resolves to in a rule message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    Below,
    Above,
}

impl Bound {
    fn as_str(self) -> &'static str {
        match self {
            Bound::Below => "below",
            Bound::Above => "above",
        }
    }
}

/// Test whether `value` lies outside the band. Returns the side that
/// tripped, or `None` when both edges are unset (open-open band) or the
/// value sits inside the band, edges included.
///
/// Boundary behaviour: a value *equal* to `min` or `max` is treated as
/// inside the band. The gate is inclusive on both edges. Authors
/// picture `accept.min = 8000` as "less than 8 Mbps is unacceptable,
/// 8 Mbps exactly is fine".
///
/// Zero-width bands (`min == max == X`) are useful as a recommendation
/// target — every value except `X` trips, with the side picked by
/// which direction `value` falls relative to `X`.
fn trip<T: PartialOrd + Copy>(band: &Band<T>, value: T) -> Option<Bound> {
    if let Some(min) = band.min
        && value < min
    {
        return Some(Bound::Below);
    }
    if let Some(max) = band.max
        && value > max
    {
        return Some(Bound::Above);
    }
    None
}

/// Render an fps band's message. Templates can reference
/// `{fps[:.N]}`, `{min[:.N]}`, `{max[:.N]}`, `{target[:.N]}`, `{bound}`.
/// `{min}` / `{max}` fall back to `0.0` when the corresponding edge is
/// absent — a rule file that uses them must have set both.
fn render_fps_band(band: &Band<f64>, target: f64, fps: f64, bound: Bound) -> String {
    let min_v = band.min.unwrap_or(0.0);
    let max_v = band.max.unwrap_or(0.0);
    render(
        &band.message,
        &[
            ("fps", Sub::Float(fps)),
            ("min", Sub::Float(min_v)),
            ("max", Sub::Float(max_v)),
            ("target", Sub::Float(target)),
            ("bound", Sub::Str(bound.as_str().to_owned())),
        ],
    )
}

/// Render a duration band's message. Templates can reference
/// `{duration[:.N]}`, `{min[:.N]}`, `{max[:.N]}`, `{target[:.N]}`,
/// `{bound}`. `duration_secs` is the value in seconds, the same unit
/// the JSON ranges are written in.
fn render_duration_band(band: &Band<f64>, target: f64, duration_secs: f64, bound: Bound) -> String {
    let min_v = band.min.unwrap_or(0.0);
    let max_v = band.max.unwrap_or(0.0);
    render(
        &band.message,
        &[
            ("duration", Sub::Float(duration_secs)),
            ("min", Sub::Float(min_v)),
            ("max", Sub::Float(max_v)),
            ("target", Sub::Float(target)),
            ("bound", Sub::Str(bound.as_str().to_owned())),
        ],
    )
}

/// Render a bitrate band's message. User-facing units are megabits, so
/// we expose `{min_mbps}` / `{max_mbps}` / `{target_mbps}` (integer
/// division of the kbps values) alongside `{bitrate}` (the
/// already-formatted current value) and `{bound}`.
fn render_bitrate_band(
    band: &Band<u64>,
    target_kbps: u64,
    bitrate_kbps: u64,
    bound: Bound,
) -> String {
    let min_v = band.min.unwrap_or(0);
    let max_v = band.max.unwrap_or(0);
    render(
        &band.message,
        &[
            ("bitrate", Sub::Str(format_bitrate(bitrate_kbps))),
            ("min_mbps", Sub::Str((min_v / 1000).to_string())),
            ("max_mbps", Sub::Str((max_v / 1000).to_string())),
            ("target_mbps", Sub::Str((target_kbps / 1000).to_string())),
            ("bound", Sub::Str(bound.as_str().to_owned())),
        ],
    )
}

/// Render a resolution band's message. Templates can reference
/// `{width}`, `{height}`, `{min}`, `{max}`, `{bound}`. `{min}` / `{max}`
/// are the pixel-count edges; `0` if absent.
fn render_resolution_band(band: &Band<u64>, width: u32, height: u32, bound: Bound) -> String {
    let min_v = band.min.unwrap_or(0);
    let max_v = band.max.unwrap_or(0);
    render(
        &band.message,
        &[
            ("width", Sub::Str(width.to_string())),
            ("height", Sub::Str(height.to_string())),
            ("min", Sub::Str(min_v.to_string())),
            ("max", Sub::Str(max_v.to_string())),
            ("bound", Sub::Str(bound.as_str().to_owned())),
        ],
    )
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
pub fn device_info_rows(
    info: &VideoInfo,
    signatures: &[DeviceEncoderSignature],
) -> Vec<(&'static str, String)> {
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
        if let Some(sig) = signatures
            .iter()
            .find(|sig| needle.contains(sig.needle.as_str()))
        {
            make = sig.vendor_label.clone();
            // Strip vendor tokens from the encoder string; what's left is
            // the model. If nothing's left (encoder was just the vendor
            // name), fall back to the full encoder so the row shows
            // something.
            let remainder = software
                .split_whitespace()
                .filter(|w| !w.eq_ignore_ascii_case(&sig.vendor_label))
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
pub fn detect_telemetry(
    info_streams: &[(ffmpeg_next::media::Type, [u8; 4])],
    rules: &TelemetryRules,
) -> Option<String> {
    info_streams.iter().find_map(|(medium, tag)| match medium {
        ffmpeg_next::media::Type::Data => rules.tags.iter().find_map(|t| match t.fourcc_bytes() {
            Some(bytes) if bytes == *tag => Some(t.label.clone()),
            _ => None,
        }),
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
fn looks_like_action_camera(info: &VideoInfo, rules: &TelemetryRules) -> bool {
    let has_action_keyword = |s: &str| {
        let n = s.to_ascii_lowercase();
        rules
            .action_camera_keywords
            .iter()
            .any(|kw| n.contains(kw.as_str()))
    };
    info.metadata.iter().any(|(_, v)| has_action_keyword(v))
}

fn provenance_warning(p: &Provenance, rules: &ProvenanceRules) -> Option<String> {
    match p {
        Provenance::CameraOriginal => None,
        Provenance::Reencoded { tool } => Some(render(
            &rules.messages.reencoded_warning,
            &[("tool", Sub::Str(tool.clone()))],
        )),
        Provenance::Stripped => Some(rules.messages.stripped_warning.clone()),
    }
}

/// Probe a video file using ffmpeg-next and validate against target parameters
pub async fn validate_video(
    path: &Path,
    rules: Arc<VideoRules>,
) -> Result<VideoValidationResult, VideoValidationError> {
    let path = path.to_path_buf();

    tokio::task::spawn_blocking(move || probe_and_validate(&path, &rules))
        .await
        .map_err(|e| VideoValidationError::ProbeFailed(e.to_string()))?
}

fn probe_and_validate(
    path: &Path,
    rules: &VideoRules,
) -> Result<VideoValidationResult, VideoValidationError> {
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
    let telemetry = detect_telemetry(&collect_data_streams(&ictx), &rules.telemetry);

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
    let provenance = classify_provenance(&ictx, &rules.provenance);

    // Soft fps band.
    if info.fps > 0.0
        && let Some(side) = trip(&rules.numeric.fps.recommend, info.fps)
    {
        warnings.push(render_fps_band(
            &rules.numeric.fps.recommend,
            rules.numeric.fps.target,
            info.fps,
            side,
        ));
    }

    // Soft resolution band. Same machinery as the accept band; a
    // zero-width recommend (`min == max`) effectively says "we want
    // exactly this resolution" and warns on any deviation.
    let pixels = u64::from(width) * u64::from(height);
    if pixels > 0
        && let Some(side) = trip(&rules.numeric.resolution.recommend, pixels)
    {
        warnings.push(render_resolution_band(
            &rules.numeric.resolution.recommend,
            width,
            height,
            side,
        ));
    }

    // Soft bitrate band.
    if bitrate_kbps > 0
        && let Some(side) = trip(&rules.numeric.bitrate_kbps.recommend, bitrate_kbps)
    {
        warnings.push(render_bitrate_band(
            &rules.numeric.bitrate_kbps.recommend,
            rules.numeric.bitrate_kbps.target,
            bitrate_kbps,
            side,
        ));
    }

    // Soft duration band.
    if duration_secs > 0.0
        && let Some(side) = trip(&rules.numeric.duration_seconds.recommend, duration_secs)
    {
        warnings.push(render_duration_band(
            &rules.numeric.duration_seconds.recommend,
            rules.numeric.duration_seconds.target,
            duration_secs,
            side,
        ));
    }

    if !warnings.is_empty() {
        warnings.push(render(
            &rules.camera_settings_guide_footer,
            &[("url", Sub::Str(rules.camera_settings_guide_url.clone()))],
        ));
    }

    // Provenance soft warning. Recommend-band-equivalent for provenance —
    // emitted on every non-camera-original row, and the UI colours it
    // alongside the other warn-tier lines. The acceptance check below
    // produces its own (error-tier) reason separately.
    if let Some(msg) = provenance_warning(&provenance, &rules.provenance) {
        warnings.push(msg);
    }

    // Action-camera-shaped clip with no telemetry: warn but don't reject.
    // DJI / GoPro recordings normally carry a per-frame data stream
    // (djmd / gpmd) that downstream evidence-handling relies on; an
    // action-camera clip without one is usually the output of a re-encode
    // that stripped the data stream while preserving the encoder tag.
    if info.telemetry.is_none() && looks_like_action_camera(&info, &rules.telemetry) {
        warnings.push(rules.telemetry.missing_telemetry_warning.clone());
    }

    let acceptance = classify_acceptance(&info, &provenance, rules);

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

    fn rules() -> Arc<VideoRules> {
        VideoRules::embedded()
    }

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

        let result = validate_video(&path, rules()).await;

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
        let r = rules();
        // iPhone-style: `make=Apple` is present alongside an encoder tag
        // that would otherwise look like a re-encode (Apple writes
        // `com.apple.quicktime.software`). Camera fingerprint wins.
        let v = classify_from_signals(true, Some("HandBrake 1.6.1"), &r.provenance);
        assert_eq!(v, Provenance::CameraOriginal);
    }

    #[test]
    fn provenance_reencoded_when_known_signature() {
        let r = rules();
        let v = classify_from_signals(false, Some("Lavf60.16.100"), &r.provenance);
        match v {
            Provenance::Reencoded { tool } => assert_eq!(tool, "Lavf60.16.100"),
            other => panic!("expected Reencoded, got {other:?}"),
        }
    }

    #[test]
    fn provenance_reencoded_case_insensitive_and_trimmed() {
        let r = rules();
        let v = classify_from_signals(false, Some("  HandBrake 1.6.1 "), &r.provenance);
        match v {
            Provenance::Reencoded { tool } => assert_eq!(tool, "HandBrake 1.6.1"),
            other => panic!("expected Reencoded, got {other:?}"),
        }
    }

    #[test]
    fn provenance_stripped_when_no_metadata() {
        let r = rules();
        let v = classify_from_signals(false, None, &r.provenance);
        assert_eq!(v, Provenance::Stripped);
    }

    #[test]
    fn provenance_stripped_when_encoder_unknown() {
        let r = rules();
        // An encoder string we don't recognize. We can't claim
        // "re-encoded by X", but the camera fingerprint is still gone.
        let v = classify_from_signals(false, Some("MysteryToolPro 9.9"), &r.provenance);
        assert_eq!(v, Provenance::Stripped);
    }

    fn full_info(width: u32, height: u32, fps: f64, bitrate_kbps: u64) -> VideoInfo {
        full_info_dur(width, height, fps, bitrate_kbps, 60.0)
    }

    fn full_info_dur(
        width: u32,
        height: u32,
        fps: f64,
        bitrate_kbps: u64,
        duration_secs: f64,
    ) -> VideoInfo {
        VideoInfo {
            width,
            height,
            fps,
            bitrate_kbps,
            codec: "h264".into(),
            audio_codec: "aac".into(),
            duration_secs,
            format: "mp4".into(),
            metadata: Vec::new(),
            telemetry: None,
        }
    }

    #[test]
    fn acceptance_clean_1440p_30fps_15mbps_passes() {
        let r = rules();
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_blocks_below_bitrate_floor() {
        let r = rules();
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 6_000),
            &Provenance::CameraOriginal,
            &r,
        );
        match v {
            Acceptance::Rejected { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("Bitrate")), "{reasons:?}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn acceptance_blocks_above_bitrate_ceiling() {
        // New ceiling: 50 Mbps. 60 Mbps must reject.
        let r = rules();
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 60_000),
            &Provenance::CameraOriginal,
            &r,
        );
        match v {
            Acceptance::Rejected { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("above")), "{reasons:?}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn acceptance_blocks_below_duration_floor() {
        // Default accept floor: 10s. A 5s clip must reject.
        let r = rules();
        let v = classify_acceptance(
            &full_info_dur(1920, 1080, 30.0, 15_000, 5.0),
            &Provenance::CameraOriginal,
            &r,
        );
        match v {
            Acceptance::Rejected { reasons } => {
                assert!(
                    reasons.iter().any(|r| r.contains("Duration")),
                    "{reasons:?}"
                );
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn acceptance_blocks_above_duration_ceiling() {
        // Default accept ceiling: 3600s (1 hour). 90 min must reject.
        let r = rules();
        let v = classify_acceptance(
            &full_info_dur(1920, 1080, 30.0, 15_000, 5400.0),
            &Provenance::CameraOriginal,
            &r,
        );
        match v {
            Acceptance::Rejected { reasons } => {
                assert!(reasons.iter().any(|r| r.contains("above")), "{reasons:?}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn acceptance_accepts_duration_at_floor_and_ceiling() {
        // 10s and 3600s both sit on band edges; equal-on-edge passes.
        let r = rules();
        for d in [10.0_f64, 3600.0_f64] {
            let v = classify_acceptance(
                &full_info_dur(1920, 1080, 30.0, 15_000, d),
                &Provenance::CameraOriginal,
                &r,
            );
            assert_eq!(v, Acceptance::Accepted, "d={d}");
        }
    }

    #[test]
    fn acceptance_skips_duration_when_unreadable() {
        // duration_secs == 0.0 → "couldn't read", don't block.
        let r = rules();
        let v = classify_acceptance(
            &full_info_dur(1920, 1080, 30.0, 15_000, 0.0),
            &Provenance::CameraOriginal,
            &r,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_passes_above_ceiling_when_max_absent() {
        // Contract: a custom rule file that omits `accept.max` on the
        // bitrate band leaves the ceiling unenforced. An otherwise-clean
        // 100 Mbps clip must pass without that rule.
        let mut r = (*rules()).clone();
        r.numeric.bitrate_kbps.accept.max = None;
        let r = Arc::new(r);
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 100_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_blocks_below_fps_floor() {
        let r = rules();
        let v = classify_acceptance(
            &full_info(2560, 1440, 18.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_blocks_above_fps_ceiling() {
        let r = rules();
        let v = classify_acceptance(
            &full_info(2560, 1440, 120.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_accepts_exactly_720p_floor() {
        // 1280×720 sits on the lower bound of the accept band
        // (720p–1440p, inclusive). Equal-on-edge passes.
        let r = rules();
        let v = classify_acceptance(
            &full_info(1280, 720, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_accepts_exactly_1440p_ceiling() {
        // 2560×1440 sits on the upper bound. Equal-on-edge passes.
        let r = rules();
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_accepts_exactly_1080p_inside_band() {
        // 1920×1080 lies strictly between 720p and 1440p in pixel
        // count, so it passes the accept band.
        let r = rules();
        let v = classify_acceptance(
            &full_info(1920, 1080, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_accepts_portrait_1080p() {
        // 1080×1920 has the same pixel count as 1920×1080, so the
        // pixel-count rule treats it identically. Rotation lives in
        // side-data, not in the dimensions stored on the stream.
        let r = rules();
        let v = classify_acceptance(
            &full_info(1080, 1920, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_rejects_above_1440p_ceiling() {
        // 4K (3840×2160) is above the new 1440p ceiling.
        let r = rules();
        let v = classify_acceptance(
            &full_info(3840, 2160, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_rejects_below_720p() {
        // SD 480p sits below the 720p floor.
        let r = rules();
        let v = classify_acceptance(
            &full_info(854, 480, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_rejects_above_1440p() {
        // 8K (7680×4320) is well past the 1440p ceiling.
        let r = rules();
        let v = classify_acceptance(
            &full_info(7680, 4320, 30.0, 15_000),
            &Provenance::CameraOriginal,
            &r,
        );
        assert!(matches!(v, Acceptance::Rejected { .. }));
    }

    #[test]
    fn acceptance_skips_when_unreadable() {
        let r = rules();
        // bitrate=0, fps=0, dimensions=0 — probe couldn't read structural
        // fields. We don't block on missing data; the unplayable path
        // already errored out earlier if the file is truly broken.
        let v = classify_acceptance(&full_info(0, 0, 0.0, 0), &Provenance::CameraOriginal, &r);
        assert_eq!(v, Acceptance::Accepted);
    }

    #[test]
    fn acceptance_blocks_reencoded_clip() {
        let r = rules();
        // Otherwise-clean clip, but the source was re-encoded — reject.
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 15_000),
            &Provenance::Reencoded {
                tool: "HandBrake 1.6.1".into(),
            },
            &r,
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
        let r = rules();
        let v = classify_acceptance(
            &full_info(2560, 1440, 30.0, 15_000),
            &Provenance::Stripped,
            &r,
        );
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
        let r = rules();
        // The exact case the user reported: encoder=DJI Osmo Nano with no
        // make/model keys. Should classify as camera-original.
        let v = classify_from_signals(false, Some("DJI Osmo Nano"), &r.provenance);
        assert_eq!(v, Provenance::CameraOriginal);
    }

    #[test]
    fn provenance_gopro_encoder_is_camera_original() {
        let r = rules();
        let v = classify_from_signals(false, Some("GoPro HERO12"), &r.provenance);
        assert_eq!(v, Provenance::CameraOriginal);
    }

    #[test]
    fn device_info_splits_dji_encoder() {
        let r = rules();
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata
            .push(("encoder".into(), "DJI Osmo Nano".into()));
        let rows = device_info_rows(&info, &r.provenance.device_encoder_signatures);
        let make = rows.iter().find(|(k, _)| *k == "Make").unwrap().1.clone();
        let model = rows.iter().find(|(k, _)| *k == "Model").unwrap().1.clone();
        assert_eq!(make, "DJI");
        assert_eq!(model, "Osmo Nano");
    }

    #[test]
    fn device_info_keeps_existing_make_when_present() {
        let r = rules();
        // When `make` and `model` are present, the encoder split must not
        // overwrite them — the explicit camera tags win.
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata
            .push(("com.apple.quicktime.make".into(), "Apple".into()));
        info.metadata
            .push(("com.apple.quicktime.model".into(), "iPhone 13 Pro".into()));
        info.metadata
            .push(("com.apple.quicktime.software".into(), "15.0.1".into()));
        let rows = device_info_rows(&info, &r.provenance.device_encoder_signatures);
        let make = rows.iter().find(|(k, _)| *k == "Make").unwrap().1.clone();
        let model = rows.iter().find(|(k, _)| *k == "Model").unwrap().1.clone();
        assert_eq!(make, "Apple");
        assert_eq!(model, "iPhone 13 Pro");
    }

    #[test]
    fn provenance_warning_message_shape() {
        let r = rules();
        assert!(provenance_warning(&Provenance::CameraOriginal, &r.provenance).is_none());

        let msg = provenance_warning(
            &Provenance::Reencoded {
                tool: "HandBrake 1.6.1".into(),
            },
            &r.provenance,
        )
        .expect("Reencoded must produce a warning");
        assert!(msg.contains("re-encoded"));
        assert!(msg.contains("HandBrake 1.6.1"));

        let msg = provenance_warning(&Provenance::Stripped, &r.provenance)
            .expect("Stripped must produce a warning");
        assert!(msg.contains("device metadata is missing"));
    }
}
