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

fn setup_video_encoder(
    octx: &mut format::context::Output,
    config: &TranscodeConfig,
    frame_rate: Rational,
    tw: u32,
    th: u32,
) -> Result<(ffmpeg_next::encoder::Video, usize), TranscodeError> {
    let name = match config.codec.as_str() {
        "hevc" | "h265" => "libx265",
        "h264" => "libx264",
        other => return Err(TranscodeError::CodecNotFound(other.to_string())),
    };
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

    let mut opts = Dictionary::new();
    opts.set("preset", &config.preset);
    opts.set("crf", &config.crf.to_string());
    opts.set(
        "x265-params",
        &format!(
            "vbv-maxrate={}:vbv-bufsize={}",
            config.max_bitrate_mbps * 1000,
            config.max_bitrate_mbps * 2000,
        ),
    );

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
