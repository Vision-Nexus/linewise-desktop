//! Video transcoding via ffmpeg-next — normalize to HEVC/H.265 + AAC.

use crate::config::TranscodeConfig;
use crate::error::TranscodeError;
use crate::models::VideoInfo;
use ffmpeg_next::{Dictionary, Rational, codec, format, media, software};
use std::path::{Path, PathBuf};

/// Result of a successful transcode operation.
#[derive(Debug)]
pub struct TranscodeResult {
    pub output_path: PathBuf,
    pub original_size: u64,
    pub transcoded_size: u64,
}

/// Progress callback: (frames_done, total_frames).
pub type ProgressFn = Box<dyn Fn(u64, u64) + Send>;

/// Rough estimate of transcoded file size (for UI display before transcoding).
pub fn estimate_transcoded_size(info: &VideoInfo, config: &TranscodeConfig) -> u64 {
    let duration_secs = info.duration_secs.max(1.0);
    let effective_bitrate_kbps = (config.max_bitrate_mbps as f64 * 1000.0 * 0.5) as u64;
    let video_bytes = effective_bitrate_kbps * (duration_secs as u64) * 1000 / 8;
    let audio_bytes = (config.audio_bitrate_kbps as u64) * (duration_secs as u64) * 1000 / 8;
    video_bytes + audio_bytes
}

/// Initialize FFmpeg library. Call once at app startup.
pub fn init() -> Result<(), TranscodeError> {
    ffmpeg_next::init().map_err(|e| {
        tracing::error!("FFmpeg init failed: {e}");
        TranscodeError::FfmpegNotAvailable
    })?;
    tracing::info!(
        "FFmpeg initialized: version {}",
        ffmpeg_next::util::version()
    );
    Ok(())
}

/// Transcode a video file to HEVC/H.265 + AAC per config.
/// Blocking — call via `spawn_blocking`.
pub fn transcode_video(
    input_path: &Path,
    info: &VideoInfo,
    config: &TranscodeConfig,
    on_progress: &dyn Fn(u64, u64),
) -> Result<TranscodeResult, TranscodeError> {
    let original_size = std::fs::metadata(input_path).map(|m| m.len()).unwrap_or(0);
    let output_path = prepare_output_path(input_path)?;

    let mut ictx = format::input(input_path).map_err(|e| enc_err(format!("Open input: {e}")))?;

    // Stream discovery
    let v_idx = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| enc_err("No video stream"))?
        .index();
    let a_idx = ictx.streams().best(media::Type::Audio).map(|s| s.index());

    // Decoders (video only — audio is stream-copied)
    let v_stream = ictx.stream(v_idx).expect("video stream");
    let v_rate = v_stream.rate();
    let mut v_dec = open_video_decoder(&v_stream)?;

    // Target resolution
    let (tw, th) = target_resolution(info, config, &v_dec);

    // Output
    let mut octx =
        format::output(&output_path).map_err(|e| enc_err(format!("Open output: {e}")))?;

    // Video encoder (re-encode to HEVC)
    let (mut v_enc, v_out) = setup_video_encoder(&mut octx, config, v_rate, tw, th)?;

    // Audio stream copy (no re-encoding)
    let a_out = if let Some(ai) = a_idx {
        let a_stream = ictx.stream(ai).expect("audio stream");
        let mut out_stream = octx
            .add_stream(codec::encoder::find(codec::Id::None))
            .map_err(|e| enc_err(format!("Add audio stream: {e}")))?;
        out_stream.set_parameters(a_stream.parameters());
        Some(out_stream.index())
    } else {
        None
    };

    // Processing contexts
    let mut scaler = build_scaler(&v_dec, tw, th)?;

    octx.write_header()
        .map_err(|e| enc_err(format!("Write header: {e}")))?;

    // Main decode → encode loop
    let total_frames = (info.duration_secs * info.fps).ceil().max(1.0) as u64;
    let mut frames_done: u64 = 0;
    let mut v_frame = ffmpeg_next::util::frame::Video::empty();

    // Audio stream time_base for remuxing
    let a_in_tb = a_idx.map(|i| ictx.stream(i).expect("audio stream").time_base());

    // Collect packets (releases ictx borrow)
    let packets: Vec<_> = ictx.packets().map(|(s, p)| (s.index(), p)).collect();

    for (stream_idx, packet) in &packets {
        if *stream_idx == v_idx {
            encode_video_packet(
                packet,
                &mut v_dec,
                &mut scaler,
                &mut v_enc,
                &mut v_frame,
                v_out,
                &mut octx,
                &mut frames_done,
                total_frames,
                on_progress,
            )?;
        } else if Some(*stream_idx) == a_idx {
            // Audio: stream copy (remux without re-encoding)
            if let (Some(a_out_idx), Some(in_tb)) = (a_out, a_in_tb) {
                let mut pkt = packet.clone();
                let out_tb = octx.stream(a_out_idx).expect("stream").time_base();
                pkt.set_stream(a_out_idx);
                pkt.rescale_ts(in_tb, out_tb);
                pkt.write_interleaved(&mut octx)
                    .map_err(|e| enc_err(format!("Write audio: {e}")))?;
            }
        }
    }

    // Flush video encoder (audio doesn't need flush — it's stream-copied)
    flush_video(&mut v_enc, v_out, &mut octx)?;

    octx.write_trailer()
        .map_err(|e| enc_err(format!("Write trailer: {e}")))?;
    on_progress(total_frames, total_frames);

    let transcoded_size = std::fs::metadata(&output_path)
        .map(|m| m.len())
        .unwrap_or(0);
    tracing::info!(
        "Transcode: {:.1}MB → {:.1}MB ({:.0}% reduction)",
        original_size as f64 / 1_048_576.0,
        transcoded_size as f64 / 1_048_576.0,
        (1.0 - transcoded_size as f64 / original_size.max(1) as f64) * 100.0,
    );

    Ok(TranscodeResult {
        output_path,
        original_size,
        transcoded_size,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────

fn enc_err(msg: impl std::fmt::Display) -> TranscodeError {
    TranscodeError::EncodingFailed(msg.to_string())
}

fn prepare_output_path(input: &Path) -> Result<PathBuf, TranscodeError> {
    let dir = std::env::temp_dir().join("linewise-transcode");
    std::fs::create_dir_all(&dir)?;
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    Ok(dir.join(format!("{stem}_transcoded.mp4")))
}

fn open_video_decoder(
    stream: &ffmpeg_next::Stream,
) -> Result<ffmpeg_next::decoder::Video, TranscodeError> {
    let ctx = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| enc_err(format!("Video decoder ctx: {e}")))?;
    ctx.decoder()
        .video()
        .map_err(|e| enc_err(format!("Video decoder: {e}")))
}

fn target_resolution(
    info: &VideoInfo,
    config: &TranscodeConfig,
    dec: &ffmpeg_next::decoder::Video,
) -> (u32, u32) {
    if info.height > config.max_height {
        let scale = config.max_height as f64 / info.height as f64;
        let w = ((info.width as f64 * scale) as u32) & !1;
        (w, config.max_height & !1)
    } else {
        (dec.width(), dec.height())
    }
}

fn build_scaler(
    dec: &ffmpeg_next::decoder::Video,
    tw: u32,
    th: u32,
) -> Result<software::scaling::Context, TranscodeError> {
    software::scaling::Context::get(
        dec.format(),
        dec.width(),
        dec.height(),
        format::Pixel::YUV420P,
        tw,
        th,
        software::scaling::Flags::BILINEAR,
    )
    .map_err(|e| enc_err(format!("Scaler: {e}")))
}

/// Which encoder family we resolved to. Each family takes a different
/// option set on `open_with`: software x264/x265 need `preset`+`crf`;
/// hardware encoders ignore those and rely on the bitrate fields on the
/// codec context, plus a small family-specific quality tweak.
///
/// VAAPI is deliberately omitted — it needs an `AVHWFramesContext` upload
/// step before `send_frame`, which this pipeline doesn't set up yet.
#[derive(Debug, Clone, Copy)]
enum EncoderKind {
    Software,
    VideoToolbox,
    Nvenc,
    Qsv,
    Amf,
}

impl EncoderKind {
    /// Ordered list of HW encoder candidates for each (codec, platform).
    /// Order reflects typical availability: on macOS VideoToolbox ships with
    /// system ffmpeg; on Windows NVENC/QSV/AMF are each tied to a GPU vendor
    /// so we probe all three and keep the first the ffmpeg build knows.
    fn hw_candidates(codec: &str) -> &'static [(&'static str, EncoderKind)] {
        match codec {
            "hevc" | "h265" => &[
                ("hevc_videotoolbox", EncoderKind::VideoToolbox),
                ("hevc_nvenc", EncoderKind::Nvenc),
                ("hevc_qsv", EncoderKind::Qsv),
                ("hevc_amf", EncoderKind::Amf),
            ],
            "h264" => &[
                ("h264_videotoolbox", EncoderKind::VideoToolbox),
                ("h264_nvenc", EncoderKind::Nvenc),
                ("h264_qsv", EncoderKind::Qsv),
                ("h264_amf", EncoderKind::Amf),
            ],
            _ => &[],
        }
    }
}

fn software_name(codec: &str) -> Result<&'static str, TranscodeError> {
    match codec {
        "hevc" | "h265" => Ok("libx265"),
        "h264" => Ok("libx264"),
        other => Err(TranscodeError::CodecNotFound(other.to_string())),
    }
}

fn find_first_available(
    candidates: &'static [(&'static str, EncoderKind)],
) -> Option<(&'static str, EncoderKind)> {
    candidates
        .iter()
        .find(|(name, _)| codec::encoder::find_by_name(name).is_some())
        .copied()
}

/// Pick an ffmpeg encoder based on the target codec and the requested HW mode.
///
/// `hw_accel` values: `"auto"` probes all HW families and falls back to
/// software if none are built in; `"none"` forces software; `"videotoolbox"`,
/// `"nvenc"`, `"qsv"`, `"amf"` force that specific family and error if the
/// ffmpeg build doesn't include it.
fn resolve_encoder(
    config: &TranscodeConfig,
) -> Result<(&'static str, EncoderKind), TranscodeError> {
    let sw = || software_name(&config.codec).map(|n| (n, EncoderKind::Software));
    let hw = EncoderKind::hw_candidates(&config.codec);

    let pick_by_kind = |wanted: EncoderKind| -> Option<(&'static str, EncoderKind)> {
        hw.iter()
            .find(|(name, kind)| {
                matches!(
                    (wanted, kind),
                    (EncoderKind::VideoToolbox, EncoderKind::VideoToolbox)
                        | (EncoderKind::Nvenc, EncoderKind::Nvenc)
                        | (EncoderKind::Qsv, EncoderKind::Qsv)
                        | (EncoderKind::Amf, EncoderKind::Amf)
                ) && codec::encoder::find_by_name(name).is_some()
            })
            .copied()
    };

    match config.hw_accel.as_str() {
        "none" => sw(),
        "videotoolbox" => pick_by_kind(EncoderKind::VideoToolbox)
            .ok_or_else(|| TranscodeError::CodecNotFound("videotoolbox".into())),
        "nvenc" => pick_by_kind(EncoderKind::Nvenc)
            .ok_or_else(|| TranscodeError::CodecNotFound("nvenc".into())),
        "qsv" => pick_by_kind(EncoderKind::Qsv)
            .ok_or_else(|| TranscodeError::CodecNotFound("qsv".into())),
        "amf" => pick_by_kind(EncoderKind::Amf)
            .ok_or_else(|| TranscodeError::CodecNotFound("amf".into())),
        // "auto" and any other value: prefer any available HW encoder, then SW.
        _ => match find_first_available(hw) {
            Some(hit) => Ok(hit),
            None => sw(),
        },
    }
}

/// Per-family encoder option dictionary. Hardware encoders ignore x264/x265
/// presets and CRF; they rely on the bitrate fields on the codec context
/// plus a family-specific quality knob. Values are chosen to favor quality
/// over latency — this is a batch transcode path, not realtime streaming.
fn encoder_options(kind: EncoderKind, config: &TranscodeConfig) -> Dictionary<'static> {
    let mut o = Dictionary::new();
    match kind {
        EncoderKind::Software => {
            o.set("preset", &config.preset);
            o.set("crf", &config.crf.to_string());
            // x265-params is only meaningful for libx265; harmless but ignored
            // by libx264, so keeping it matches historical behavior.
            o.set(
                "x265-params",
                &format!(
                    "vbv-maxrate={}:vbv-bufsize={}",
                    config.max_bitrate_mbps * 1000,
                    config.max_bitrate_mbps * 2000,
                ),
            );
        }
        EncoderKind::VideoToolbox => {
            // `realtime=0` favors quality over encoding latency.
            o.set("realtime", "0");
        }
        EncoderKind::Nvenc => {
            // NVENC presets p1..p7 run fastest→slowest/highest-quality in the
            // modern preset scheme. p5 is a good default; `rc=vbr` + the
            // codec-context bitrate fields drive output bitrate.
            o.set("preset", "p5");
            o.set("rc", "vbr");
        }
        EncoderKind::Qsv => {
            // Intel QuickSync takes libx264-style preset names. `slow` is the
            // quality-leaning default Intel recommends for non-realtime use.
            o.set("preset", "slow");
        }
        EncoderKind::Amf => {
            // AMD AMF uses `quality` as its quality-vs-speed knob; values are
            // `speed` | `balanced` | `quality`.
            o.set("quality", "quality");
            o.set("rc", "vbr_peak");
        }
    }
    o
}

fn setup_video_encoder(
    octx: &mut format::context::Output,
    config: &TranscodeConfig,
    frame_rate: Rational,
    tw: u32,
    th: u32,
) -> Result<(ffmpeg_next::encoder::Video, usize), TranscodeError> {
    let (name, kind) = resolve_encoder(config)?;
    tracing::info!("Using video encoder: {name} ({kind:?})");
    let video_codec = codec::encoder::find_by_name(name)
        .ok_or_else(|| TranscodeError::CodecNotFound(name.to_string()))?;

    let mut stream = octx
        .add_stream(video_codec)
        .map_err(|e| enc_err(format!("Add video stream: {e}")))?;
    let out_idx = stream.index();

    let ctx = codec::context::Context::new_with_codec(video_codec);
    let mut enc = ctx
        .encoder()
        .video()
        .map_err(|e| enc_err(format!("Video enc: {e}")))?;
    // time_base = inverse of frame_rate (e.g. 30fps → 1/30)
    // libx265 requires a valid timebase; decoder.time_base() is often 0/0 or codec-level
    let enc_time_base = if frame_rate.0 > 0 && frame_rate.1 > 0 {
        Rational::new(frame_rate.1, frame_rate.0)
    } else {
        Rational::new(1, 30) // fallback
    };
    enc.set_width(tw);
    enc.set_height(th);
    enc.set_format(format::Pixel::YUV420P);
    enc.set_time_base(enc_time_base);
    enc.set_frame_rate(Some(frame_rate));
    enc.set_bit_rate(config.max_bitrate_mbps as usize * 1_000_000);
    enc.set_max_bit_rate(config.max_bitrate_mbps as usize * 1_000_000);

    let opts = encoder_options(kind, config);

    let opened = enc
        .open_with(opts)
        .map_err(|e| enc_err(format!("Open video enc: {e}")))?;
    stream.set_parameters(&opened);
    Ok((opened, out_idx))
}

#[allow(clippy::too_many_arguments)]
fn encode_video_packet(
    packet: &ffmpeg_next::Packet,
    decoder: &mut ffmpeg_next::decoder::Video,
    scaler: &mut software::scaling::Context,
    encoder: &mut ffmpeg_next::encoder::Video,
    decoded: &mut ffmpeg_next::util::frame::Video,
    out_idx: usize,
    octx: &mut format::context::Output,
    frames_done: &mut u64,
    total_frames: u64,
    on_progress: &dyn Fn(u64, u64),
) -> Result<(), TranscodeError> {
    decoder
        .send_packet(packet)
        .map_err(|e| enc_err(format!("Send video pkt: {e}")))?;

    while decoder.receive_frame(decoded).is_ok() {
        let mut scaled = ffmpeg_next::util::frame::Video::empty();
        scaler
            .run(decoded, &mut scaled)
            .map_err(|e| enc_err(format!("Scale: {e}")))?;
        scaled.set_pts(decoded.pts());
        scaled.set_kind(decoded.kind());

        encoder
            .send_frame(&scaled)
            .map_err(|e| enc_err(format!("Enc video: {e}")))?;
        drain_encoder_packets(encoder, encoder.time_base(), out_idx, octx)?;

        *frames_done += 1;
        if frames_done.is_multiple_of(30) || *frames_done == total_frames {
            on_progress(*frames_done, total_frames);
        }
    }
    Ok(())
}

fn drain_encoder_packets(
    encoder: &mut ffmpeg_next::encoder::Video,
    in_tb: Rational,
    out_idx: usize,
    octx: &mut format::context::Output,
) -> Result<(), TranscodeError> {
    let out_tb = octx.stream(out_idx).expect("stream").time_base();
    let mut pkt = ffmpeg_next::Packet::empty();
    while encoder.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(out_idx);
        pkt.rescale_ts(in_tb, out_tb);
        pkt.write_interleaved(octx)
            .map_err(|e| enc_err(format!("Write video: {e}")))?;
    }
    Ok(())
}

fn flush_video(
    encoder: &mut ffmpeg_next::encoder::Video,
    out_idx: usize,
    octx: &mut format::context::Output,
) -> Result<(), TranscodeError> {
    encoder.send_eof().ok();
    drain_encoder_packets(encoder, encoder.time_base(), out_idx, octx)
}
