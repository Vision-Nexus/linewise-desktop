//! Video transcoding via ffmpeg-next.
//!
//! Two-stage pipeline:
//!   1. Decode source → encode to HEVC/AAC, muxed as HLS segments under a
//!      per-task scratch directory. Segments are 6-second MPEG-TS files named
//!      `media-hd%010d.ts`. A killed process loses at most the partial segment
//!      currently being written.
//!   2. Stream-copy (`-c copy`) all finished segments into a single MP4 with
//!      `+faststart`.
//!
//! Resume: the filesystem is the source of truth. On resume we enumerate the
//! existing segments, delete the tail (may be truncated), seek the input past
//! the duration they cover, and continue the encode with `start_number=N`
//! and `hls_flags=append_list`. See `linewise-api/scripts/video_process.py`
//! lines 226–265 for the identical pattern used in production.
//!
//! Three PoC-validated quirks are load-bearing here and worth calling out:
//!   - The HLS muxer AVOption is `start_number`, not `hls_start_number`.
//!     Misspelled keys are silently ignored; we materialize the leftover
//!     dictionary from `write_header_with` and treat non-empty as a hard error.
//!   - `ffmpeg-next` has no helper to open an input with a named format, so we
//!     drop to `ffmpeg_next::sys::av_find_input_format` + raw `avformat_open_input`
//!     for the concat demuxer. One small `unsafe` block.
//!   - HEVC streams in both HLS and MP4 outputs get `codec_tag = "hvc1"`. Without
//!     the explicit tag, VideoToolbox writes `hev1`, which Safari accepts but
//!     web `<video>` broadly rejects.

use crate::config::TranscodeConfig;
use crate::error::TranscodeError;
use crate::models::VideoInfo;
use ffmpeg_next::{
    ChannelLayout, Dictionary, Error as FfError, Packet, Rational, codec, filter, format, media,
    picture, sys,
};
use std::ffi::CString;
use std::fs;
use std::path::{Path, PathBuf};

/// HLS segment target length, seconds. Matches production's 6-second shape
/// from `video_process.py`.
pub(crate) const SEGMENT_SECONDS: f64 = 6.0;
/// Every segment file starts with this prefix; production uses the same
/// `media-hd` so sorted-glob-as-numeric-sort works with the `%010d` padding.
pub(crate) const SEG_FILENAME_PREFIX: &str = "media-hd";
const PLAYLIST_FILENAME: &str = "media-hd.m3u8";

/// Result of a successful transcode operation.
#[derive(Debug)]
pub struct TranscodeResult {
    pub output_path: PathBuf,
    pub original_size: u64,
    pub transcoded_size: u64,
}

/// Display-matrix side-data on the source video stream, copied verbatim.
/// Nine 32-bit ints, the AV_PKT_DATA_DISPLAYMATRIX format used by GoPro,
/// DJI, and iPhone-portrait clips to encode a 90° / 180° / 270° rotation
/// without baking it into pixels. We treat it as opaque bytes — the helper
/// reads it on the input and reapplies it to the final MP4 stream.
#[derive(Debug, Clone, Copy)]
pub struct DisplayMatrix(pub [i32; 9]);

/// Snapshot of what transcoding features are usable in the current process.
/// Returned by [`probe_availability`] for the UI to render the settings pane.
#[derive(Debug, Clone)]
pub struct AvailabilityReport {
    pub ffmpeg: bool,
    pub available_hw: Vec<HwKind>,
}

/// Hardware encoder families the settings pane surfaces to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwKind {
    VideoToolbox,
    Nvenc,
    Qsv,
    Amf,
}

impl HwKind {
    pub fn as_config_str(self) -> &'static str {
        match self {
            HwKind::VideoToolbox => "videotoolbox",
            HwKind::Nvenc => "nvenc",
            HwKind::Qsv => "qsv",
            HwKind::Amf => "amf",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            HwKind::VideoToolbox => "VideoToolbox (macOS)",
            HwKind::Nvenc => "NVENC (NVIDIA)",
            HwKind::Qsv => "QuickSync (Intel)",
            HwKind::Amf => "AMF (AMD)",
        }
    }
}

/// Rough estimate of transcoded file size for the staging UI.
pub fn estimate_transcoded_size(info: &VideoInfo, config: &TranscodeConfig) -> u64 {
    let duration_secs = info.duration_secs.max(1.0);
    // VBR targets the average, not the peak. Use the target directly;
    // the old 0.5-factor on max was a hack back when bit_rate == max_bit_rate
    // caused systematic undershoot.
    let effective_bitrate_kbps = (config.target_bitrate_mbps as u64) * 1000;
    let video_bytes = effective_bitrate_kbps * (duration_secs as u64) * 1000 / 8;
    let audio_bytes = (config.audio_bitrate_kbps as u64) * (duration_secs as u64) * 1000 / 8;
    video_bytes + audio_bytes
}

/// Initialize FFmpeg library. Call once at app startup.
#[tracing::instrument(skip_all)]
pub fn init() -> Result<(), TranscodeError> {
    // `warn!` (not `error!`) on init failure: the app falls back to "transcoding
    // disabled" — see `main.rs`. Sentry promotes `error!` into incidents and
    // the absence of a transcoder is an expected, recoverable startup condition.
    ffmpeg_next::init().map_err(|e| {
        tracing::warn!("FFmpeg init failed: {e}");
        TranscodeError::FfmpegNotAvailable
    })?;
    tracing::info!(
        "FFmpeg initialized: version {}",
        ffmpeg_next::util::version()
    );
    Ok(())
}

/// Probe what transcoding features the current process has. The UI uses this
/// to gate the settings pane — see [`transcode_settings.rs`] in lw-app.
#[tracing::instrument(skip_all, fields(codec = %config.codec))]
pub fn probe_availability(config: &TranscodeConfig) -> AvailabilityReport {
    // `init()` is idempotent — calling it from here is safe if the app hasn't
    // done it already, but in practice `main.rs` calls it at startup and we
    // just re-check via `version()`.
    let ffmpeg = ffmpeg_next::init().is_ok();
    let mut available_hw = Vec::new();
    if ffmpeg {
        for (name, kind) in EncoderKind::hw_candidates(&config.codec) {
            if codec::encoder::find_by_name(name).is_some()
                && let Some(hw) = EncoderKind::to_hw_kind(*kind)
            {
                available_hw.push(hw);
            }
        }
    }
    AvailabilityReport {
        ffmpeg,
        available_hw,
    }
}

/// Transcode a video file to HEVC/H.265 + AAC per config. Blocking — call via
/// `spawn_blocking`. Resumable: if a partially-transcoded HLS scratch directory
/// already exists under the per-task scratch root, the encode continues from
/// the first missing segment.
#[tracing::instrument(skip_all, fields(
    filename = input_path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
    codec = %config.codec,
    original_size = tracing::field::Empty,
))]
pub fn transcode_video(
    input_path: &Path,
    info: &VideoInfo,
    config: &TranscodeConfig,
    on_progress: &dyn Fn(u64, u64),
) -> Result<TranscodeResult, TranscodeError> {
    let original_size = std::fs::metadata(input_path).map(|m| m.len()).unwrap_or(0);
    tracing::Span::current().record("original_size", original_size);
    let (scratch_dir, output_path) = prepare_paths(input_path)?;
    fs::create_dir_all(&scratch_dir).map_err(TranscodeError::Io)?;

    // Read the source video stream's display-matrix once, before the HLS
    // intermediate strips it. MPEG-TS doesn't carry display-matrix natively,
    // so we hold the value in memory and reapply it on the final MP4 below.
    let rotation = probe_display_matrix(input_path);
    if rotation.is_some() {
        tracing::info!("Source carries a display-matrix; will reapply it to the final MP4");
    }

    let resume = detect_resume_point(&scratch_dir)?;
    if resume.start_seg > 0 {
        tracing::info!(
            "Transcode resume: {} existing segments ({}s covered)",
            resume.start_seg,
            resume.resume_seconds
        );
    }

    encode_to_hls(input_path, &scratch_dir, info, config, &resume, on_progress)?;
    concat_to_mp4(&scratch_dir, &output_path, rotation)?;

    let transcoded_size = std::fs::metadata(&output_path)
        .map(|m| m.len())
        .unwrap_or(0);
    tracing::info!(
        "Transcode: {:.1}MB → {:.1}MB ({:.0}% reduction)",
        original_size as f64 / 1_048_576.0,
        transcoded_size as f64 / 1_048_576.0,
        (1.0 - transcoded_size as f64 / original_size.max(1) as f64) * 100.0,
    );

    // Clean up scratch once the final MP4 is in hand.
    let _ = fs::remove_dir_all(&scratch_dir);

    Ok(TranscodeResult {
        output_path,
        original_size,
        transcoded_size,
    })
}

// ── Path layout ─────────────────────────────────────────────────────────────

/// Compute scratch directory (for HLS segments during encode) and the final
/// output MP4 path.
#[doc(hidden)]
pub fn prepare_paths(input: &Path) -> Result<(PathBuf, PathBuf), TranscodeError> {
    let base = std::env::temp_dir().join("linewise-transcode");
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    // Scratch dir is per-input-stem so two concurrent transcodes don't collide,
    // and so resume picks up the right directory on relaunch.
    let scratch = base.join(format!("{stem}_hls"));
    let output = base.join(format!("{stem}_transcoded.mp4"));
    fs::create_dir_all(&base).map_err(TranscodeError::Io)?;
    Ok((scratch, output))
}

// ── Resume detection ────────────────────────────────────────────────────────

#[doc(hidden)]
#[derive(Debug)]
pub struct ResumePoint {
    pub start_seg: u64,
    pub resume_seconds: f64,
}

/// Walk the scratch dir, drop the tail segment (may be truncated from a crash),
/// and return how many segments survived plus the total video-time they cover.
#[doc(hidden)]
pub fn detect_resume_point(scratch_dir: &Path) -> Result<ResumePoint, TranscodeError> {
    if !scratch_dir.exists() {
        return Ok(ResumePoint {
            start_seg: 0,
            resume_seconds: 0.0,
        });
    }
    let mut segs: Vec<PathBuf> = fs::read_dir(scratch_dir)
        .map_err(TranscodeError::Io)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(SEG_FILENAME_PREFIX) && n.ends_with(".ts"))
                .unwrap_or(false)
        })
        .collect();
    segs.sort();

    // Drop the tail: if an encode was killed mid-segment, libav may have
    // flushed partial bytes to that file. Deleting it is cheaper than trying
    // to probe whether it's salvageable.
    if let Some(last) = segs.last().cloned() {
        tracing::info!(
            "Dropping possibly-truncated tail segment: {}",
            last.display()
        );
        let _ = fs::remove_file(&last);
        segs.pop();
    }

    let n = segs.len() as u64;
    Ok(ResumePoint {
        start_seg: n,
        resume_seconds: n as f64 * SEGMENT_SECONDS,
    })
}

// ── Stage 1: encode to HLS segments ─────────────────────────────────────────

#[doc(hidden)]
pub fn encode_to_hls(
    input_path: &Path,
    scratch_dir: &Path,
    info: &VideoInfo,
    config: &TranscodeConfig,
    resume: &ResumePoint,
    on_progress: &dyn Fn(u64, u64),
) -> Result<(), TranscodeError> {
    let mut ictx = format::input(input_path).map_err(|e| enc_err(format!("Open input: {e}")))?;

    if resume.resume_seconds > 0.0 {
        let ts = (resume.resume_seconds * sys::AV_TIME_BASE as f64) as i64;
        ictx.seek(ts, ..ts)
            .map_err(|e| enc_err(format!("Seek to {}s failed: {e}", resume.resume_seconds)))?;
    }

    let v_idx = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| enc_err("No video stream"))?
        .index();
    let a_idx = ictx.streams().best(media::Type::Audio).map(|s| s.index());

    let (v_rate, v_in_tb, mut v_dec) = {
        let v_stream = ictx.stream(v_idx).expect("video stream");
        let rate = v_stream.rate();
        let tb = v_stream.time_base();
        let dec = open_video_decoder(&v_stream)?;
        (rate, tb, dec)
    };

    let (tw, th) = target_resolution(info, config, &v_dec);

    let playlist_path = scratch_dir.join(PLAYLIST_FILENAME);
    let mut octx = format::output_as(&playlist_path, "hls")
        .map_err(|e| enc_err(format!("Alloc HLS output: {e}")))?;

    let (name, kind) = resolve_encoder(config)?;
    tracing::info!("Using video encoder: {name} ({kind:?})");
    let video_codec = codec::encoder::find_by_name(name)
        .ok_or_else(|| TranscodeError::CodecNotFound(name.to_string()))?;

    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);
    let target_pix = encoder_pixel_format(kind);

    // Build video encoder.
    let mut v_enc_ctx = codec::context::Context::new_with_codec(video_codec)
        .encoder()
        .video()
        .map_err(|e| enc_err(format!("Video enc ctx: {e}")))?;
    v_enc_ctx.set_width(tw);
    v_enc_ctx.set_height(th);
    v_enc_ctx.set_format(target_pix);
    v_enc_ctx.set_frame_rate(Some(v_rate));
    let enc_time_base = if v_rate.0 > 0 && v_rate.1 > 0 {
        Rational::new(v_rate.1, v_rate.0)
    } else {
        Rational::new(1, 30)
    };
    v_enc_ctx.set_time_base(enc_time_base);
    // Split target from peak. When bit_rate == max_bit_rate, VideoToolbox
    // treats the stream as a tight ceiling and systematically undershoots
    // the target (PoC on 4K60p input: 7 Mbps actual against a 10 Mbps tight
    // cap, SSIM 0.943). With peak set to 2× target we land close to the
    // target with a quality bump (10 Mbps actual, SSIM 0.956) while still
    // staying under the user's 20 Mbps hard ceiling.
    v_enc_ctx.set_bit_rate(config.target_bitrate_mbps as usize * 1_000_000);
    v_enc_ctx.set_max_bit_rate(config.max_bitrate_mbps as usize * 1_000_000);
    let fps = if v_rate.1 > 0 {
        v_rate.0 as f64 / v_rate.1 as f64
    } else {
        30.0
    };
    let gop = ((fps * SEGMENT_SECONDS).round() as u32).max(1);
    v_enc_ctx.set_gop(gop);
    if global_header {
        v_enc_ctx.set_flags(codec::Flags::GLOBAL_HEADER);
    }

    let v_enc = v_enc_ctx
        .open_with(encoder_options(kind, config))
        .map_err(|e| enc_err(format!("Open video enc: {e}")))?;

    // Add video output stream and tag it hvc1 for HEVC.
    let mut vst = octx
        .add_stream(video_codec)
        .map_err(|e| enc_err(format!("Add video stream: {e}")))?;
    vst.set_parameters(&v_enc);
    vst.set_time_base(enc_time_base);
    if config.codec == "hevc" || config.codec == "h265" {
        // SAFETY: setting codec_tag on an output stream before write_header.
        // Standard pattern used by ffmpeg-next's own transcode-x264 example.
        unsafe {
            let tag = u32::from_le_bytes(*b"hvc1");
            (*vst.parameters().as_mut_ptr()).codec_tag = tag;
        }
    }
    let v_out_idx = vst.index();

    // Audio: decode the source track and re-encode to AAC-LC. Stream-copy
    // worked for AAC-in-MP4 sources but fails on MKV containers that carry
    // TrueHD / DTS-HD / FLAC / Opus — none of those are legal in MPEG-TS,
    // so `write_interleaved` on their packets returns EINVAL. AAC-LC is
    // the canonical HLS audio codec and handles every input the desktop
    // client is likely to see.
    let mut audio_pipeline = if let Some(ai) = a_idx {
        Some(build_audio_pipeline(
            &ictx,
            &mut octx,
            ai,
            config,
            global_header,
        )?)
    } else {
        None
    };
    let a_out_idx = audio_pipeline.as_ref().map(|p| p.out_idx);

    // HLS muxer options. CRITICAL: the AVOption name is `start_number`, not
    // `hls_start_number`. A typo is silently ignored — the leftover dictionary
    // from write_header_with is the only signal, and we fail loudly below if
    // any key comes back unconsumed.
    let seg_pattern = scratch_dir.join(format!("{SEG_FILENAME_PREFIX}%010d.ts"));
    let seg_pattern_str = seg_pattern
        .to_str()
        .ok_or_else(|| enc_err("Non-UTF8 scratch path"))?;
    let mut hls_opts = Dictionary::new();
    hls_opts.set("hls_time", &format!("{}", SEGMENT_SECONDS as u32));
    hls_opts.set("hls_list_size", "0");
    hls_opts.set("hls_segment_filename", seg_pattern_str);
    hls_opts.set("hls_segment_type", "mpegts");
    hls_opts.set("start_number", &format!("{}", resume.start_seg));
    if resume.start_seg > 0 {
        hls_opts.set("hls_flags", "append_list");
    }

    let leftover_vec: Vec<(String, String)> = {
        let leftover = octx
            .write_header_with(hls_opts)
            .map_err(|e| enc_err(format!("HLS write_header: {e}")))?;
        leftover
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    if !leftover_vec.is_empty() {
        return Err(enc_err(format!(
            "HLS muxer rejected options (check AVOption names): {leftover_vec:?}"
        )));
    }

    // Pull output timebases (libav may have rewritten them during write_header).
    let v_out_tb = octx
        .stream(v_out_idx)
        .expect("video out stream")
        .time_base();
    if let Some(p) = audio_pipeline.as_mut()
        && let Some(idx) = a_out_idx
    {
        p.out_tb = octx.stream(idx).expect("audio out stream").time_base();
    }
    let a_in_tb = a_idx.and_then(|i| ictx.stream(i).map(|s| s.time_base()));

    // Build scaler if input pix_fmt / dimensions differ from encoder target.
    let need_scale = v_dec.format() != target_pix || v_dec.width() != tw || v_dec.height() != th;
    let mut scaler = if need_scale {
        Some(
            ffmpeg_next::software::scaling::Context::get(
                v_dec.format(),
                v_dec.width(),
                v_dec.height(),
                target_pix,
                tw,
                th,
                ffmpeg_next::software::scaling::Flags::BILINEAR,
            )
            .map_err(|e| enc_err(format!("Scaler: {e}")))?,
        )
    } else {
        None
    };

    // Encode loop — streams packets one at a time rather than collecting into
    // a Vec, so memory stays bounded on long inputs (see PoC stress test:
    // peak RSS ~640 MB regardless of video length).
    let total_frames = (info.duration_secs * info.fps).ceil().max(1.0) as u64;
    // When resuming we skip the already-covered part; report progress from
    // that offset so the UI bar continues where it left off.
    let mut frames_done = (resume.resume_seconds * info.fps).ceil() as u64;
    let mut v_enc = v_enc;
    // Monotonic DTS guard for the audio remux path. After rescale_ts
    // rounds the input timebase into the output timebase, two distinct
    // input packets can land on the same output DTS — which fails the
    // HLS muxer's strict-monotonic check. Track the last emitted DTS
    // and bump collisions by +1 tick, same pattern ffmpeg's CLI uses
    // when it emits "non monotonic DTS ... clipping".
    let mut last_audio_dts: Option<i64> = None;

    for (stream, mut packet) in ictx.packets() {
        let sidx = stream.index();
        if sidx == v_idx {
            process_video_packet(
                &packet,
                &mut v_dec,
                scaler.as_mut(),
                &mut v_enc,
                v_in_tb,
                v_out_tb,
                v_out_idx,
                &mut octx,
                &mut frames_done,
                total_frames,
                resume.resume_seconds,
                on_progress,
            )?;
        } else if Some(sidx) == a_idx
            && let Some(pipeline) = audio_pipeline.as_mut()
            && let Some(tb_in) = a_in_tb
        {
            // Drop audio packets that predate the resume point by more than
            // half a segment — same shape the PoC used. Small intentional
            // overlap is fine; re-muxing already-covered audio isn't.
            if resume.resume_seconds > 0.0
                && let Some(pts) = packet.pts()
            {
                let t = pts as f64 * f64::from(tb_in);
                if t < resume.resume_seconds - (SEGMENT_SECONDS / 2.0) {
                    continue;
                }
            }
            // rescale into the decoder's time base so pts from different
            // containers land on a consistent scale before filter / encode.
            packet.rescale_ts(tb_in, pipeline.dec_tb);
            process_audio_packet(&packet, pipeline, &mut octx, &mut last_audio_dts)?;
        }
    }

    // Flush decoder + encoder.
    v_dec.send_eof().ok();
    let mut frame = ffmpeg_next::util::frame::Video::empty();
    while v_dec.receive_frame(&mut frame).is_ok() {
        let pts = frame.pts();
        if let Some(sc) = scaler.as_mut() {
            let mut scaled = ffmpeg_next::util::frame::Video::empty();
            sc.run(&frame, &mut scaled)
                .map_err(|e| enc_err(format!("Scale: {e}")))?;
            scaled.set_pts(pts);
            scaled.set_kind(picture::Type::None);
            v_enc.send_frame(&scaled).ok();
        } else {
            frame.set_kind(picture::Type::None);
            v_enc.send_frame(&frame).ok();
        }
        drain_video(&mut v_enc, v_in_tb, v_out_tb, v_out_idx, &mut octx)?;
    }
    v_enc.send_eof().ok();
    drain_video(&mut v_enc, v_in_tb, v_out_tb, v_out_idx, &mut octx)?;

    // Flush the audio pipeline: push EOF through the decoder, pump any
    // remaining frames through the filter graph, then drain the encoder.
    if let Some(pipeline) = audio_pipeline.as_mut() {
        pipeline.dec.send_eof().ok();
        flush_audio_pipeline(pipeline, &mut octx, &mut last_audio_dts)?;
    }

    on_progress(total_frames, total_frames);
    octx.write_trailer()
        .map_err(|e| enc_err(format!("HLS write_trailer: {e}")))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_video_packet(
    packet: &Packet,
    decoder: &mut ffmpeg_next::decoder::Video,
    mut scaler: Option<&mut ffmpeg_next::software::scaling::Context>,
    encoder: &mut ffmpeg_next::encoder::Video,
    v_in_tb: Rational,
    v_out_tb: Rational,
    v_out_idx: usize,
    octx: &mut format::context::Output,
    frames_done: &mut u64,
    total_frames: u64,
    resume_seconds: f64,
    on_progress: &dyn Fn(u64, u64),
) -> Result<(), TranscodeError> {
    decoder
        .send_packet(packet)
        .map_err(|e| enc_err(format!("Send video pkt: {e}")))?;

    let mut frame = ffmpeg_next::util::frame::Video::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        // After seek, libav hands us frames that may still fall before the
        // resume boundary. Drop them — otherwise the encoder re-emits content
        // already covered by existing segments.
        if resume_seconds > 0.0
            && let Some(pts) = frame.pts()
        {
            let t = pts as f64 * f64::from(v_in_tb);
            if t < resume_seconds - (SEGMENT_SECONDS / 2.0) {
                continue;
            }
        }

        let pts = frame.pts();
        if let Some(sc) = scaler.as_mut() {
            let mut scaled = ffmpeg_next::util::frame::Video::empty();
            sc.run(&frame, &mut scaled)
                .map_err(|e| enc_err(format!("Scale: {e}")))?;
            scaled.set_pts(pts);
            scaled.set_kind(picture::Type::None);
            encoder
                .send_frame(&scaled)
                .map_err(|e| enc_err(format!("Enc video: {e}")))?;
        } else {
            frame.set_kind(picture::Type::None);
            encoder
                .send_frame(&frame)
                .map_err(|e| enc_err(format!("Enc video: {e}")))?;
        }
        drain_video(encoder, v_in_tb, v_out_tb, v_out_idx, octx)?;

        *frames_done += 1;
        if frames_done.is_multiple_of(30) || *frames_done == total_frames {
            on_progress(*frames_done, total_frames);
        }
    }
    Ok(())
}

fn drain_video(
    encoder: &mut ffmpeg_next::encoder::Video,
    in_tb: Rational,
    out_tb: Rational,
    out_idx: usize,
    octx: &mut format::context::Output,
) -> Result<(), TranscodeError> {
    let mut pkt = Packet::empty();
    while encoder.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(out_idx);
        pkt.rescale_ts(in_tb, out_tb);
        pkt.write_interleaved(octx)
            .map_err(|e| enc_err(format!("Write video: {e}")))?;
    }
    Ok(())
}

// ── Stage 2: concat segments into MP4 ───────────────────────────────────────

#[doc(hidden)]
pub fn concat_to_mp4(
    scratch_dir: &Path,
    output_path: &Path,
    rotation: Option<DisplayMatrix>,
) -> Result<(), TranscodeError> {
    let mut segs: Vec<PathBuf> = fs::read_dir(scratch_dir)
        .map_err(TranscodeError::Io)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(SEG_FILENAME_PREFIX) && n.ends_with(".ts"))
                .unwrap_or(false)
        })
        .collect();
    segs.sort();
    if segs.is_empty() {
        return Err(enc_err("No .ts segments to concat"));
    }
    tracing::info!("Concatenating {} HLS segments into MP4", segs.len());

    // Concat demuxer reads a playlist file with `file '<path>'` lines.
    let concat_list = scratch_dir.join("concat.txt");
    let mut contents = String::new();
    for seg in &segs {
        let abs = fs::canonicalize(seg).map_err(TranscodeError::Io)?;
        let path_str = abs
            .to_str()
            .ok_or_else(|| enc_err("Non-UTF8 segment path"))?;
        contents.push_str("file '");
        contents.push_str(path_str);
        contents.push_str("'\n");
    }
    fs::write(&concat_list, contents).map_err(TranscodeError::Io)?;

    let mut ictx = open_concat_input(&concat_list)?;

    let mut octx = format::output_as(output_path, "mp4")
        .map_err(|e| enc_err(format!("Alloc MP4 output: {e}")))?;

    // Map input streams to output. Stream-copy (no re-encode).
    struct Mapping {
        in_tb: Rational,
        out_tb: Rational,
        out_index: usize,
    }
    let mut mappings: std::collections::HashMap<usize, Mapping> = std::collections::HashMap::new();
    let mut rotation_attached = false;
    for ist in ictx.streams() {
        let medium = ist.parameters().medium();
        if medium != media::Type::Video && medium != media::Type::Audio {
            continue;
        }
        let mut ost = octx
            .add_stream(codec::encoder::find(codec::Id::None))
            .map_err(|e| enc_err(format!("Add output stream: {e}")))?;
        ost.set_parameters(ist.parameters());
        let codec_id = ist.parameters().id();
        if codec_id == codec::Id::HEVC {
            // SAFETY: forcing hvc1 tag before header-write. Without this,
            // VideoToolbox writes hev1 and browsers reject the MP4.
            unsafe {
                let tag = u32::from_le_bytes(*b"hvc1");
                (*ost.parameters().as_mut_ptr()).codec_tag = tag;
            }
        } else {
            // SAFETY: codec_tag=0 asks the MP4 muxer to pick a compatible tag.
            unsafe {
                (*ost.parameters().as_mut_ptr()).codec_tag = 0;
            }
        }
        let ist_index = ist.index();
        let in_tb = ist.time_base();
        let out_index = ost.index();
        if medium == media::Type::Video
            && !rotation_attached
            && let Some(matrix) = rotation
        {
            attach_display_matrix(&mut ost, matrix)?;
            rotation_attached = true;
        }
        ost.set_time_base(Rational::new(1, 90_000));
        mappings.insert(
            ist_index,
            Mapping {
                in_tb,
                out_tb: Rational::new(1, 90_000),
                out_index,
            },
        );
    }

    let mut mp4_opts = Dictionary::new();
    mp4_opts.set("movflags", "+faststart");
    let leftover_vec: Vec<(String, String)> = {
        let leftover = octx
            .write_header_with(mp4_opts)
            .map_err(|e| enc_err(format!("MP4 write_header: {e}")))?;
        leftover
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    if !leftover_vec.is_empty() {
        return Err(enc_err(format!(
            "MP4 muxer rejected options: {leftover_vec:?}"
        )));
    }

    // Refresh output time bases; libav may have rewritten them.
    for m in mappings.values_mut() {
        m.out_tb = octx.stream(m.out_index).expect("out stream").time_base();
    }

    for (stream, mut packet) in ictx.packets() {
        let Some(mapping) = mappings.get(&stream.index()) else {
            continue;
        };
        packet.rescale_ts(mapping.in_tb, mapping.out_tb);
        packet.set_position(-1);
        packet.set_stream(mapping.out_index);
        packet.write_interleaved(&mut octx).ok();
    }

    octx.write_trailer()
        .map_err(|e| enc_err(format!("MP4 write_trailer: {e}")))?;
    Ok(())
}

/// Open the concat demuxer on the given playlist file. ffmpeg-next has no
/// `input_with_name(path, format_name, dict)` helper, so this uses the raw
/// `ffmpeg_next::sys` bindings to set the format + `safe=0` option.
fn open_concat_input(concat_list: &Path) -> Result<format::context::Input, TranscodeError> {
    let path_cstr = CString::new(
        concat_list
            .as_os_str()
            .to_str()
            .ok_or_else(|| enc_err("Non-UTF8 concat list path"))?,
    )
    .map_err(|e| enc_err(format!("Concat list CString: {e}")))?;
    let name = CString::new("concat").expect("static");
    let key_safe = CString::new("safe").expect("static");
    let val_zero = CString::new("0").expect("static");

    // SAFETY: ffmpeg-next's public API has no helper that accepts both a
    // format-name override and an option dictionary, so this block does the
    // equivalent of `avformat_open_input(&mut ps, path, "concat", {safe: 0})`
    // by hand. All pointers are freed on error.
    unsafe {
        let input_format = sys::av_find_input_format(name.as_ptr());
        if input_format.is_null() {
            return Err(enc_err("concat demuxer not compiled into libavformat"));
        }
        let mut opts: *mut sys::AVDictionary = std::ptr::null_mut();
        sys::av_dict_set(&mut opts, key_safe.as_ptr(), val_zero.as_ptr(), 0);

        let mut ps: *mut sys::AVFormatContext = std::ptr::null_mut();
        let ret = sys::avformat_open_input(
            &mut ps,
            path_cstr.as_ptr(),
            input_format as *mut _,
            &mut opts,
        );
        sys::av_dict_free(&mut opts);
        if ret < 0 {
            return Err(enc_err(format!(
                "avformat_open_input(concat) failed: {ret}"
            )));
        }
        let r2 = sys::avformat_find_stream_info(ps, std::ptr::null_mut());
        if r2 < 0 {
            sys::avformat_close_input(&mut ps);
            return Err(enc_err(format!("find_stream_info(concat) failed: {r2}")));
        }
        Ok(format::context::Input::wrap(ps))
    }
}

// ── Display-matrix side-data preservation ──────────────────────────────────

/// Read the display-matrix side-data off the source video stream, if any.
/// Walks the codecpar `coded_side_data` array directly because ffmpeg-next's
/// `Stream::side_data()` returns a packet-side-data iterator that doesn't
/// expose mutable access for forwarding to the output, and copying out the
/// raw 36 bytes here keeps the read path self-contained.
fn read_display_matrix(stream: &ffmpeg_next::Stream) -> Option<DisplayMatrix> {
    // SAFETY: stream codecpar lives as long as the input format context;
    // we read at most 36 bytes from a known-shape AVPacketSideData entry.
    unsafe {
        let mut params = stream.parameters();
        let par = params.as_mut_ptr();
        let count = (*par).nb_coded_side_data;
        let head = (*par).coded_side_data;
        if head.is_null() || count <= 0 {
            return None;
        }
        for i in 0..count as isize {
            let entry = head.offset(i);
            if (*entry).type_ != sys::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX {
                continue;
            }
            if (*entry).size < std::mem::size_of::<[i32; 9]>() || (*entry).data.is_null() {
                return None;
            }
            let mut out = [0i32; 9];
            std::ptr::copy_nonoverlapping((*entry).data as *const i32, out.as_mut_ptr(), 9);
            return Some(DisplayMatrix(out));
        }
        None
    }
}

/// Probe the source file for a video-stream display-matrix. Returns `None`
/// for files without rotation metadata, files we can't open, or anything
/// libav rejects — a missing matrix is the common case, not an error.
pub fn probe_display_matrix(input_path: &Path) -> Option<DisplayMatrix> {
    let ictx = format::input(input_path).ok()?;
    let v_stream = ictx.streams().best(media::Type::Video)?;
    read_display_matrix(&v_stream)
}

/// Attach a display-matrix entry to the output stream's codecpar
/// `coded_side_data` array. Must be called before `write_header_with` —
/// libavformat reads codecpar side-data exactly once at header time.
fn attach_display_matrix(
    stream: &mut ffmpeg_next::format::stream::StreamMut,
    matrix: DisplayMatrix,
) -> Result<(), TranscodeError> {
    const SIZE: usize = std::mem::size_of::<[i32; 9]>();
    // SAFETY: av_malloc/av_packet_side_data_add transfer ownership of the
    // buffer to the side-data array, which is freed by avcodec_parameters_free
    // when the output context is dropped. We bail and free locally on the
    // single failure path before that hand-off.
    unsafe {
        let mut params = stream.parameters();
        let par = params.as_mut_ptr();
        let buf = sys::av_malloc(SIZE) as *mut i32;
        if buf.is_null() {
            return Err(enc_err("av_malloc(display-matrix) returned null"));
        }
        std::ptr::copy_nonoverlapping(matrix.0.as_ptr(), buf, 9);
        let added = sys::av_packet_side_data_add(
            &mut (*par).coded_side_data,
            &mut (*par).nb_coded_side_data,
            sys::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
            buf as *mut std::ffi::c_void,
            SIZE,
            0,
        );
        if added.is_null() {
            sys::av_free(buf as *mut std::ffi::c_void);
            return Err(enc_err("av_packet_side_data_add(display-matrix) failed"));
        }
    }
    Ok(())
}

// ── Encoder selection ───────────────────────────────────────────────────────

fn enc_err(msg: impl std::fmt::Display) -> TranscodeError {
    TranscodeError::EncodingFailed(msg.to_string())
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

// ── Audio pipeline (decode → filter → AAC encode → mux) ─────────────────────

/// Holds every piece of mutable state the audio re-encode path needs across
/// iterations of the main packet loop. Packaging them up keeps the loop body
/// flat and the per-packet call site readable.
struct AudioPipeline {
    dec: ffmpeg_next::decoder::Audio,
    /// Filter graph: `abuffer` (source) → resample/format-convert/FIFO →
    /// `abuffersink`. Rebuilding 1024-sample AAC frames is the whole reason
    /// we use a graph here instead of `swresample` alone.
    graph: filter::Graph,
    enc: ffmpeg_next::encoder::audio::Encoder,
    /// Output stream index in the HLS muxer.
    out_idx: usize,
    /// Decoder-side time base. Input packets are rescaled into this space
    /// before being pushed to the decoder so the filter graph sees a
    /// consistent scale regardless of the source container.
    dec_tb: Rational,
    /// Muxer-side time base after `write_header` has possibly rewritten it.
    out_tb: Rational,
}

fn open_audio_decoder(
    stream: &ffmpeg_next::Stream,
) -> Result<ffmpeg_next::decoder::Audio, TranscodeError> {
    let ctx = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| enc_err(format!("Audio decoder ctx: {e}")))?;
    ctx.decoder()
        .audio()
        .map_err(|e| enc_err(format!("Audio decoder: {e}")))
}

/// AAC-LC accepts any sample rate the encoder reports, but the common MPEG-TS
/// players and browsers expect 44.1 or 48 kHz. Keep those rates as-is,
/// otherwise normalize to 48 kHz.
fn aac_target_rate(src_rate: u32) -> u32 {
    match src_rate {
        44_100 | 48_000 => src_rate,
        _ => 48_000,
    }
}

/// AAC-LC encoders in ffmpeg accept mono and stereo. Anything else (5.1,
/// 7.1, Dolby Atmos beds) gets downmixed to stereo by the filter graph's
/// automatic channel conversion.
fn aac_target_layout(src_layout: ChannelLayout) -> ChannelLayout {
    if src_layout.channels() == 1 {
        ChannelLayout::MONO
    } else {
        ChannelLayout::STEREO
    }
}

fn build_audio_pipeline(
    ictx: &format::context::Input,
    octx: &mut format::context::Output,
    a_idx: usize,
    config: &TranscodeConfig,
    global_header: bool,
) -> Result<AudioPipeline, TranscodeError> {
    let a_stream = ictx.stream(a_idx).expect("audio stream");
    let mut dec = open_audio_decoder(&a_stream)?;
    // Normalize the decoder time base to (1, sample_rate). abuffer requires
    // a valid time_base, and some containers leave it at 0/0.
    let dec_rate = dec.rate().max(1);
    let dec_tb = Rational::new(1, dec_rate as i32);
    dec.set_packet_time_base(dec_tb);

    let aac_codec = codec::encoder::find(codec::Id::AAC).ok_or_else(|| {
        TranscodeError::CodecNotFound("aac (libavcodec built without AAC encoder)".into())
    })?;

    let target_rate = aac_target_rate(dec_rate);
    let target_layout = aac_target_layout(dec.channel_layout());
    // AAC-LC requires planar float samples.
    let target_format = format::Sample::F32(format::sample::Type::Planar);
    let target_tb = Rational::new(1, target_rate as i32);

    let mut enc_ctx = codec::context::Context::new_with_codec(aac_codec)
        .encoder()
        .audio()
        .map_err(|e| enc_err(format!("Audio enc ctx: {e}")))?;
    enc_ctx.set_bit_rate(config.audio_bitrate_kbps as usize * 1000);
    enc_ctx.set_rate(target_rate as i32);
    enc_ctx.set_format(target_format);
    enc_ctx.set_channel_layout(target_layout);
    enc_ctx.set_time_base(target_tb);
    if global_header {
        enc_ctx.set_flags(codec::Flags::GLOBAL_HEADER);
    }

    let enc = enc_ctx
        .open_as(aac_codec)
        .map_err(|e| enc_err(format!("Open AAC enc: {e}")))?;

    let graph = build_audio_filter_graph(&dec, &enc, dec_tb)?;

    let mut ost = octx
        .add_stream(aac_codec)
        .map_err(|e| enc_err(format!("Add audio stream: {e}")))?;
    ost.set_parameters(&enc);
    ost.set_time_base(target_tb);
    // codec_tag=0 lets the muxer pick the canonical tag for AAC.
    // SAFETY: setting codec_tag on an output stream before write_header.
    unsafe {
        (*ost.parameters().as_mut_ptr()).codec_tag = 0;
    }
    let out_idx = ost.index();

    Ok(AudioPipeline {
        dec,
        graph,
        enc,
        out_idx,
        dec_tb,
        out_tb: target_tb,
    })
}

/// Build an abuffer → (implicit resample/format/channel conversion) → abuffersink
/// graph that feeds the AAC encoder. The `anull` filter spec leaves the
/// processing chain empty; libavfilter inserts the necessary `aresample` and
/// channel-mix filters automatically from the mismatch between the source
/// definition (the `abuffer` args) and the sink's requested format / rate /
/// layout. This is the pattern ffmpeg-next's own `transcode-audio` example
/// uses and is the canonical way to rebuild fixed-size frames for encoders
/// like AAC-LC that require exactly `frame_size()` samples per call.
fn build_audio_filter_graph(
    dec: &ffmpeg_next::decoder::Audio,
    enc: &ffmpeg_next::encoder::audio::Encoder,
    dec_tb: Rational,
) -> Result<filter::Graph, TranscodeError> {
    let mut graph = filter::Graph::new();

    let args = format!(
        "time_base={}/{}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
        dec_tb.numerator(),
        dec_tb.denominator(),
        dec.rate(),
        dec.format().name(),
        dec.channel_layout().bits(),
    );

    let abuffer = filter::find("abuffer").ok_or_else(|| enc_err("abuffer filter missing"))?;
    let abuffersink =
        filter::find("abuffersink").ok_or_else(|| enc_err("abuffersink filter missing"))?;

    graph
        .add(&abuffer, "in", &args)
        .map_err(|e| enc_err(format!("abuffer add: {e}")))?;
    graph
        .add(&abuffersink, "out", "")
        .map_err(|e| enc_err(format!("abuffersink add: {e}")))?;

    // FFmpeg 8 dropped `sample_fmts` / `sample_rates` / `channel_layouts` from
    // the runtime-settable AVOption set on `abuffersink`, so the pre-8 idiom of
    // `sink.set_sample_format(...)` now errors with
    // "Option 'sample_fmts' is not a runtime option". Instead, thread the
    // format constraints through an `aformat` filter inside the parse spec —
    // libavfilter inserts `aresample` / channel-mix upstream of `aformat`
    // automatically when the format differs.
    let aformat_spec = format!(
        "aformat=sample_fmts={}:sample_rates={}:channel_layouts=0x{:x}",
        enc.format().name(),
        enc.rate(),
        enc.channel_layout().bits(),
    );

    graph
        .output("in", 0)
        .map_err(|e| enc_err(format!("graph output: {e}")))?
        .input("out", 0)
        .map_err(|e| enc_err(format!("graph input: {e}")))?
        .parse(&aformat_spec)
        .map_err(|e| enc_err(format!("graph parse: {e}")))?;
    graph
        .validate()
        .map_err(|e| enc_err(format!("graph validate: {e}")))?;

    // Unless the encoder tolerates variable frame sizes (AAC-LC does not),
    // pin the sink to emit fixed-size frames matching the encoder's
    // frame_size so `send_frame` never rejects a short frame.
    let needs_fixed_size = enc.codec().is_some_and(|c| {
        !c.capabilities()
            .contains(codec::Capabilities::VARIABLE_FRAME_SIZE)
    });
    if needs_fixed_size && enc.frame_size() > 0 {
        graph
            .get("out")
            .expect("out node")
            .sink()
            .set_frame_size(enc.frame_size());
    }

    Ok(graph)
}

/// Push one decoded packet through the audio pipeline: decoder → filter
/// graph → AAC encoder → muxer.
fn process_audio_packet(
    packet: &Packet,
    pipeline: &mut AudioPipeline,
    octx: &mut format::context::Output,
    last_audio_dts: &mut Option<i64>,
) -> Result<(), TranscodeError> {
    pipeline
        .dec
        .send_packet(packet)
        .map_err(|e| enc_err(format!("Send audio pkt: {e}")))?;
    drain_audio_decoder(pipeline, octx, last_audio_dts)
}

/// Pull every ready frame out of the decoder, feed it into the filter graph,
/// collect every filtered frame that pops out, push those into the AAC
/// encoder, and drain its packets into the muxer.
fn drain_audio_decoder(
    pipeline: &mut AudioPipeline,
    octx: &mut format::context::Output,
    last_audio_dts: &mut Option<i64>,
) -> Result<(), TranscodeError> {
    let mut decoded = ffmpeg_next::util::frame::Audio::empty();
    while pipeline.dec.receive_frame(&mut decoded).is_ok() {
        // libav's filter graph wants frame pts in the graph's input time
        // base, which matches the decoder's. `timestamp()` returns the
        // best-effort pts the decoder assigned.
        let ts = decoded.timestamp();
        decoded.set_pts(ts);
        pipeline
            .graph
            .get("in")
            .expect("in node")
            .source()
            .add(&decoded)
            .map_err(|e| enc_err(format!("Filter add: {e}")))?;
        pump_audio_filter_to_encoder(pipeline, octx, last_audio_dts)?;
    }
    Ok(())
}

fn pump_audio_filter_to_encoder(
    pipeline: &mut AudioPipeline,
    octx: &mut format::context::Output,
    last_audio_dts: &mut Option<i64>,
) -> Result<(), TranscodeError> {
    let mut filtered = ffmpeg_next::util::frame::Audio::empty();
    loop {
        match pipeline
            .graph
            .get("out")
            .expect("out node")
            .sink()
            .frame(&mut filtered)
        {
            Ok(()) => {}
            // Normal "no frame ready" / "input side closed" conditions —
            // stop draining, let the caller decide what to do next.
            Err(FfError::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
                return Ok(());
            }
            Err(FfError::Eof) => return Ok(()),
            // Anything else is a real failure — propagate it instead of
            // silently eating it.
            Err(e) => return Err(enc_err(format!("Filter sink: {e}"))),
        }
        pipeline
            .enc
            .send_frame(&filtered)
            .map_err(|e| enc_err(format!("Enc audio: {e}")))?;
        drain_audio_encoder(pipeline, octx, last_audio_dts)?;
    }
}

fn drain_audio_encoder(
    pipeline: &mut AudioPipeline,
    octx: &mut format::context::Output,
    last_audio_dts: &mut Option<i64>,
) -> Result<(), TranscodeError> {
    let mut pkt = Packet::empty();
    while pipeline.enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(pipeline.out_idx);
        pkt.rescale_ts(pipeline.enc.time_base(), pipeline.out_tb);
        pkt.set_position(-1);
        enforce_monotonic_dts(&mut pkt, last_audio_dts);
        pkt.write_interleaved(octx)
            .map_err(|e| enc_err(format!("Write audio: {e}")))?;
    }
    Ok(())
}

/// Flush the audio chain at EOF. Caller must have already called
/// `pipeline.dec.send_eof()` so the decoder is in draining mode.
fn flush_audio_pipeline(
    pipeline: &mut AudioPipeline,
    octx: &mut format::context::Output,
    last_audio_dts: &mut Option<i64>,
) -> Result<(), TranscodeError> {
    drain_audio_decoder(pipeline, octx, last_audio_dts)?;
    // Close the graph's input side so trailing samples buffered for
    // frame-size alignment get emitted rather than dropped.
    pipeline
        .graph
        .get("in")
        .expect("in node")
        .source()
        .flush()
        .map_err(|e| enc_err(format!("Filter flush: {e}")))?;
    pump_audio_filter_to_encoder(pipeline, octx, last_audio_dts)?;
    pipeline.enc.send_eof().ok();
    drain_audio_encoder(pipeline, octx, last_audio_dts)
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

/// Which encoder family we resolved to. Hardware encoders take NV12 input and
/// a family-specific quality knob; software encoders take YUV420P + preset/crf.
///
/// VAAPI is deliberately omitted — it needs an `AVHWFramesContext` upload step
/// before `send_frame` that this pipeline doesn't set up yet.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    Software,
    VideoToolbox,
    Nvenc,
    Qsv,
    Amf,
}

impl EncoderKind {
    // The BtbN FFmpeg Windows build registers every encoder regardless of
    // platform, so `find_by_name("h264_videotoolbox")` returns Some(_) on
    // Windows — but opening it fails with EPERM at avcodec_open2 time
    // because the kernel extension isn't there. Filter by target_os first
    // so we don't even try impossible families.
    #[cfg(target_os = "macos")]
    const HW_FAMILIES_HEVC: &'static [(&'static str, EncoderKind)] =
        &[("hevc_videotoolbox", EncoderKind::VideoToolbox)];
    #[cfg(target_os = "macos")]
    const HW_FAMILIES_H264: &'static [(&'static str, EncoderKind)] =
        &[("h264_videotoolbox", EncoderKind::VideoToolbox)];

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    const HW_FAMILIES_HEVC: &'static [(&'static str, EncoderKind)] = &[
        ("hevc_nvenc", EncoderKind::Nvenc),
        ("hevc_qsv", EncoderKind::Qsv),
        ("hevc_amf", EncoderKind::Amf),
    ];
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    const HW_FAMILIES_H264: &'static [(&'static str, EncoderKind)] = &[
        ("h264_nvenc", EncoderKind::Nvenc),
        ("h264_qsv", EncoderKind::Qsv),
        ("h264_amf", EncoderKind::Amf),
    ];

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    const HW_FAMILIES_HEVC: &'static [(&'static str, EncoderKind)] = &[];
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    const HW_FAMILIES_H264: &'static [(&'static str, EncoderKind)] = &[];

    pub fn hw_candidates(codec: &str) -> &'static [(&'static str, EncoderKind)] {
        match codec {
            "hevc" | "h265" => Self::HW_FAMILIES_HEVC,
            "h264" => Self::HW_FAMILIES_H264,
            _ => &[],
        }
    }

    fn to_hw_kind(self) -> Option<HwKind> {
        match self {
            EncoderKind::Software => None,
            EncoderKind::VideoToolbox => Some(HwKind::VideoToolbox),
            EncoderKind::Nvenc => Some(HwKind::Nvenc),
            EncoderKind::Qsv => Some(HwKind::Qsv),
            EncoderKind::Amf => Some(HwKind::Amf),
        }
    }
}

/// All hardware families take NV12 natively on Apple silicon / NVIDIA / Intel /
/// AMD. Software encoders take fully-planar YUV420P.
fn encoder_pixel_format(kind: EncoderKind) -> format::Pixel {
    match kind {
        EncoderKind::Software => format::Pixel::YUV420P,
        EncoderKind::VideoToolbox | EncoderKind::Nvenc | EncoderKind::Qsv | EncoderKind::Amf => {
            format::Pixel::NV12
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

/// Bump a packet's DTS (and PTS if needed) so it's strictly greater than
/// the last emitted DTS for the same stream. Two distinct input audio
/// packets can rescale onto the same output DTS after timebase rounding,
/// which trips the HLS muxer's monotonicity check. Mirrors the "non
/// monotonic DTS … clipping" fallback the ffmpeg CLI uses.
fn enforce_monotonic_dts(packet: &mut ffmpeg_next::Packet, last_dts: &mut Option<i64>) {
    let Some(dts) = packet.dts() else { return };
    match *last_dts {
        Some(prev) if dts <= prev => {
            let fixed = prev + 1;
            packet.set_dts(Some(fixed));
            if packet.pts().is_some_and(|p| p < fixed) {
                packet.set_pts(Some(fixed));
            }
            *last_dts = Some(fixed);
        }
        _ => *last_dts = Some(dts),
    }
}

/// Returns `true` if the FFmpeg build registers an encoder under `name`.
/// Registration is not the same as usability — `h264_nvenc` is registered
/// on a BtbN Windows build even on a machine without an NVIDIA card.
fn encoder_registered(name: &str) -> bool {
    codec::encoder::find_by_name(name).is_some()
}

/// Try to actually open the encoder so we know the driver is present.
/// On mismatch (e.g. nvenc on a non-NVIDIA Windows box), avcodec_open2
/// fails with EPERM / ENOENT / ENODEV and we fall through.
///
/// Probe dimensions must clear the minimum frame size each hardware
/// family enforces — AMF refuses below ~130x130, NVENC HEVC refuses
/// below 64x64, QSV is also picky on undersized inputs. 256x144
/// (16:9-ish, multiple of 16 on both axes) is accepted by every
/// family we target.
fn encoder_opens(name: &str) -> bool {
    let Some(codec) = codec::encoder::find_by_name(name) else {
        return false;
    };
    let Ok(ctx) = codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
    else {
        return false;
    };
    let mut ctx = ctx;
    ctx.set_width(256);
    ctx.set_height(144);
    ctx.set_format(format::Pixel::NV12);
    ctx.set_time_base(Rational::new(1, 30));
    ctx.set_bit_rate(1_000_000);
    ctx.open_as(codec).is_ok()
}

#[doc(hidden)]
pub fn find_first_available(
    candidates: &'static [(&'static str, EncoderKind)],
) -> Option<(&'static str, EncoderKind)> {
    // First pass: drop candidates the build doesn't even know about.
    // Second pass: actually probe-open the survivor to prove the driver
    // is there. We only probe up to two entries in practice, so the cost
    // is a few milliseconds on the first transcode.
    candidates
        .iter()
        .filter(|(name, _)| encoder_registered(name))
        .find(|(name, _)| {
            let ok = encoder_opens(name);
            if !ok {
                tracing::debug!("hw encoder {name} registered but open failed; skipping");
            }
            ok
        })
        .copied()
}

/// Pick an ffmpeg encoder. `hw_accel = "auto"` probes all HW families and
/// falls back to software; `"none"` forces software; family names force that
/// specific family and error if the ffmpeg build doesn't include it.
#[doc(hidden)]
pub fn resolve_encoder(
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
        _ => match find_first_available(hw) {
            Some(hit) => Ok(hit),
            None => sw(),
        },
    }
}

/// Per-family encoder option dictionary. Hardware encoders ignore preset/crf;
/// they rely on the bitrate fields on the codec context plus a family-specific
/// quality knob. Values favor quality over latency — this is a batch path,
/// not realtime streaming.
fn encoder_options(kind: EncoderKind, config: &TranscodeConfig) -> Dictionary<'static> {
    let mut o = Dictionary::new();
    match kind {
        EncoderKind::Software => {
            o.set("preset", &config.preset);
            o.set("crf", &config.crf.to_string());
            // `scenecut=0` forces keyframe alignment on segment boundaries
            // — required for clean HLS segmentation. libx264 ignores
            // x265-params silently, so it's a no-op there.
            o.set(
                "x265-params",
                &format!(
                    "vbv-maxrate={}:vbv-bufsize={}:scenecut=0",
                    config.max_bitrate_mbps * 1000,
                    config.max_bitrate_mbps * 2000,
                ),
            );
        }
        EncoderKind::VideoToolbox => {
            o.set("realtime", "0");
            o.set("allow_sw", "1");
            // Explicit VBR (not CBR). Pairs with the codec-context
            // bit_rate (target) / max_bit_rate (peak) pair — see PoC at
            // .claude/worktrees/agent-ae5b2cb8949f44e98/vt-bitrate/.
            o.set("constant_bit_rate", "0");
        }
        EncoderKind::Nvenc => {
            o.set("preset", "p5");
            o.set("rc", "vbr");
        }
        EncoderKind::Qsv => {
            o.set("preset", "slow");
        }
        EncoderKind::Amf => {
            o.set("quality", "quality");
            o.set("rc", "vbr_peak");
        }
    }
    o
}

/// Remove a transcoded temp artifact after a successful upload, logging
/// (but not failing) if the file is already gone.
pub fn cleanup_temp_file(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!("Failed to clean up temp file {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lw-test-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn write_segment(dir: &Path, n: u64) {
        let name = format!("{SEG_FILENAME_PREFIX}{n:010}.ts");
        std::fs::write(dir.join(name), b"").expect("write segment fixture");
    }

    #[test]
    fn resume_point_missing_dir() {
        let missing = std::env::temp_dir().join(format!("lw-no-such-{}", uuid::Uuid::new_v4()));
        let rp = detect_resume_point(&missing).expect("detect");
        assert_eq!(rp.start_seg, 0);
        assert_eq!(rp.resume_seconds, 0.0);
    }

    #[test]
    fn resume_point_empty_dir() {
        let dir = fresh_temp_dir("resume-empty");

        let rp = detect_resume_point(&dir).expect("detect");

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rp.start_seg, 0);
        assert_eq!(rp.resume_seconds, 0.0);
    }

    #[test]
    fn resume_point_drops_tail_and_ignores_unrelated() {
        let dir = fresh_temp_dir("resume-tail");
        for n in 0..5 {
            write_segment(&dir, n);
        }
        // unrelated files should not be counted or removed
        std::fs::write(dir.join("concat.txt"), b"placeholder").unwrap();
        std::fs::write(dir.join("media-hd.m3u8"), b"placeholder").unwrap();

        let rp = detect_resume_point(&dir).expect("detect");

        // 5 segments → drop tail → 4 remain
        assert_eq!(rp.start_seg, 4);
        assert!((rp.resume_seconds - 4.0 * SEGMENT_SECONDS).abs() < 1e-9);
        // tail removed
        assert!(
            !dir.join(format!("{SEG_FILENAME_PREFIX}0000000004.ts"))
                .exists()
        );
        // unrelated files preserved
        assert!(dir.join("concat.txt").exists());
        assert!(dir.join("media-hd.m3u8").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_point_single_segment_drops_to_empty() {
        let dir = fresh_temp_dir("resume-single");
        write_segment(&dir, 0);

        let rp = detect_resume_point(&dir).expect("detect");

        assert_eq!(rp.start_seg, 0);
        assert_eq!(rp.resume_seconds, 0.0);
        assert!(
            !dir.join(format!("{SEG_FILENAME_PREFIX}0000000000.ts"))
                .exists()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concat_to_mp4_empty_scratch_returns_typed_error() {
        let dir = fresh_temp_dir("concat-empty");
        let out = dir.join("out.mp4");

        let err = concat_to_mp4(&dir, &out, None).expect_err("must fail on empty scratch");

        match err {
            TranscodeError::EncodingFailed(msg) => {
                assert!(
                    msg.contains("No .ts segments to concat"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected EncodingFailed, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
