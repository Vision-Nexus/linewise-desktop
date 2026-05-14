//! Shared fixture-synthesis helpers for transcode integration tests.
//
// Cargo treats `tests/common/mod.rs` as compiled once per integration test
// binary, so any helper not used by *every* test trips the `dead_code` lint
// in at least one binary. The pragmatic fix is module-level `#[allow]` on
// dead_code and the `field_reassign_with_default` clippy lint that fires
// because building a `TranscodeConfig` field-by-field is clearer than
// spelling out every default.
#![allow(dead_code, clippy::field_reassign_with_default)]

use ffmpeg_next as ffmpeg;
use ffmpeg_next::{ChannelLayout, Rational, codec, format, picture, util::frame::Audio};
use lw_core::config::TranscodeConfig;
use lw_core::models::VideoInfo;
use std::path::{Path, PathBuf};

pub const FIXTURE_FPS: i32 = 30;
pub const FIXTURE_DURATION_SECS: f64 = 12.0;
pub const FIXTURE_WIDTH: u32 = 320;
pub const FIXTURE_HEIGHT: u32 = 240;

pub fn fresh_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lw-test-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

pub fn ffmpeg_init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("ffmpeg init");
    });
}

/// Build a deterministic libx264-encoded MP4 fixture. Y-plane content varies
/// per frame so the encoder cannot trivially compress everything to the same
/// size. The clip is intentionally small (320×240, 30fps, 12 s by default)
/// to keep test runtime under a second on a software encoder.
pub fn synthesize_video_fixture(path: &Path, total_frames: u64) {
    let codec = codec::encoder::find_by_name("libx264")
        .expect("libx264 must be available — it is a build prerequisite");

    let mut octx = format::output(&path.to_path_buf()).expect("alloc mp4 output");
    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    let time_base = Rational::new(1, FIXTURE_FPS);
    let frame_rate = Rational::new(FIXTURE_FPS, 1);

    let mut enc_ctx = codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .expect("video encoder ctx");
    enc_ctx.set_width(FIXTURE_WIDTH);
    enc_ctx.set_height(FIXTURE_HEIGHT);
    enc_ctx.set_format(format::Pixel::YUV420P);
    enc_ctx.set_time_base(time_base);
    enc_ctx.set_frame_rate(Some(frame_rate));
    enc_ctx.set_gop(FIXTURE_FPS as u32);
    enc_ctx.set_bit_rate(800_000);
    if global_header {
        enc_ctx.set_flags(codec::Flags::GLOBAL_HEADER);
    }

    let mut x264_opts = ffmpeg::Dictionary::new();
    x264_opts.set("preset", "ultrafast");
    x264_opts.set("tune", "zerolatency");
    let mut enc = enc_ctx
        .open_with(x264_opts)
        .expect("open libx264 with ultrafast preset");

    let mut ost = octx.add_stream(codec).expect("add video stream");
    ost.set_parameters(&enc);
    ost.set_time_base(time_base);
    let ost_index = ost.index();

    octx.write_header().expect("mp4 write_header");
    let ost_tb = octx.stream(ost_index).unwrap().time_base();

    let mut f =
        ffmpeg::util::frame::Video::new(format::Pixel::YUV420P, FIXTURE_WIDTH, FIXTURE_HEIGHT);
    for i in 0..total_frames {
        let y_val = ((i * 7) % 256) as u8;
        f.data_mut(0).fill(y_val);
        f.data_mut(1).fill(128);
        f.data_mut(2).fill(128);
        f.set_pts(Some(i as i64));
        f.set_kind(picture::Type::None);

        enc.send_frame(&f).expect("send frame");
        drain_video_packets(&mut enc, &mut octx, ost_index, time_base, ost_tb);
    }
    enc.send_eof().ok();
    drain_video_packets(&mut enc, &mut octx, ost_index, time_base, ost_tb);

    octx.write_trailer().expect("mp4 write_trailer");
}

fn drain_video_packets(
    enc: &mut ffmpeg::encoder::Video,
    octx: &mut format::context::Output,
    ost_index: usize,
    in_tb: Rational,
    out_tb: Rational,
) {
    let mut pkt = ffmpeg::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(ost_index);
        pkt.rescale_ts(in_tb, out_tb);
        pkt.write_interleaved(octx)
            .expect("write video packet to fixture");
    }
}

/// Build an AAC-only m4a (no video stream) for the audio-only failure path.
///
/// The native AAC encoder shipped with Ubuntu's libavcodec 58 only accepts
/// the FLTP (planar f32) sample format that the codec advertises in
/// `sample_fmts()`, and only at the rates listed in `supported_rates()`.
/// Older fixtures hard-coded 48 kHz and `F32 Planar`, which works on
/// macOS Homebrew (FFmpeg 7) but fails with EINVAL on Ubuntu 22.04
/// (FFmpeg 4.4) because of a subtler ABI mismatch in how `send_frame`
/// reads the channel layout. Pulling the encoder's preferred sample
/// format and rate directly from the codec descriptor sidesteps the
/// per-distribution divergence.
pub fn synthesize_audio_only_fixture(path: &Path) {
    let codec = codec::encoder::find(codec::Id::AAC)
        .expect("AAC encoder must be available — it is a build prerequisite");

    let audio_codec = codec.audio().expect("AAC codec descriptor");
    let sample_format = audio_codec
        .formats()
        .and_then(|mut it| it.next())
        .unwrap_or(format::Sample::F32(format::sample::Type::Planar));
    let sample_rate: i32 = audio_codec
        .rates()
        .and_then(|mut it| it.next())
        .unwrap_or(48_000);

    let mut octx = format::output(&path.to_path_buf()).expect("alloc m4a output");
    let global_header = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    let time_base = Rational::new(1, sample_rate);

    let mut enc_ctx = codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()
        .expect("audio encoder ctx");
    enc_ctx.set_bit_rate(96_000);
    enc_ctx.set_rate(sample_rate);
    enc_ctx.set_format(sample_format);
    enc_ctx.set_channel_layout(ChannelLayout::STEREO);
    enc_ctx.set_time_base(time_base);
    if global_header {
        enc_ctx.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut enc = enc_ctx.open_as(codec).expect("open aac encoder");

    let mut ost = octx.add_stream(codec).expect("add audio stream");
    ost.set_parameters(&enc);
    ost.set_time_base(time_base);
    let ost_index = ost.index();

    octx.write_header().expect("m4a write_header");
    let ost_tb = octx.stream(ost_index).unwrap().time_base();

    // Native AAC on Linux refuses any frame smaller than `frame_size`
    // (returns EINVAL), unlike libfdk-aac which is more lenient. Round
    // total down so every send_frame carries a full frame.
    let frame_size = enc.frame_size().max(1024) as usize;
    let total_samples = (sample_rate as usize / frame_size) * frame_size;
    let mut written = 0usize;
    let mut pts: i64 = 0;
    while written < total_samples {
        let mut af = Audio::new(sample_format, frame_size, ChannelLayout::STEREO);
        af.set_rate(sample_rate as u32);
        af.set_pts(Some(pts));
        for ch in 0..af.planes() {
            af.data_mut(ch).fill(0);
        }
        enc.send_frame(&af).expect("send audio frame");
        drain_audio_packets(&mut enc, &mut octx, ost_index, time_base, ost_tb);
        pts += frame_size as i64;
        written += frame_size;
    }
    enc.send_eof().ok();
    drain_audio_packets(&mut enc, &mut octx, ost_index, time_base, ost_tb);

    octx.write_trailer().expect("m4a write_trailer");
}

fn drain_audio_packets(
    enc: &mut ffmpeg::encoder::Audio,
    octx: &mut format::context::Output,
    ost_index: usize,
    in_tb: Rational,
    out_tb: Rational,
) {
    let mut pkt = ffmpeg::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(ost_index);
        pkt.rescale_ts(in_tb, out_tb);
        pkt.write_interleaved(octx)
            .expect("write audio packet to fixture");
    }
}

pub fn probe_duration_secs(path: &Path) -> f64 {
    let ictx = format::input(&path.to_path_buf()).expect("open output");
    ictx.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
}

/// Read the container-level `codec_tag` of the first video stream as a
/// 4-byte ASCII tag (e.g. `"hvc1"`, `"hev1"`, `"avc1"`). Reaches into the
/// raw codec parameters via `unsafe`, mirroring the write path in
/// transcode.rs that sets the tag on the way out.
pub fn read_video_codec_tag(path: &Path) -> [u8; 4] {
    let ictx = format::input(&path.to_path_buf()).expect("open output");
    let stream = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .expect("output should have a video stream");
    let params = stream.parameters();
    // SAFETY: we only read the four-byte codec_tag field; the parameters
    // pointer is valid for the lifetime of the input context.
    let tag_u32 = unsafe { (*params.as_ptr()).codec_tag };
    tag_u32.to_le_bytes()
}

pub fn synthetic_video_info(duration_secs: f64) -> VideoInfo {
    VideoInfo {
        width: FIXTURE_WIDTH,
        height: FIXTURE_HEIGHT,
        fps: FIXTURE_FPS as f64,
        bitrate_kbps: 800,
        codec: "h264".into(),
        audio_codec: String::new(),
        duration_secs,
        format: "mp4".into(),
    }
}

pub fn audio_only_video_info(duration_secs: f64) -> VideoInfo {
    VideoInfo {
        width: 0,
        height: 0,
        fps: 0.0,
        bitrate_kbps: 0,
        codec: String::new(),
        audio_codec: "aac".into(),
        duration_secs,
        format: "mov".into(),
    }
}

pub fn forced_software_config() -> TranscodeConfig {
    let mut cfg = TranscodeConfig::default();
    cfg.hw_accel = "none".into();
    cfg.preset = "ultrafast".into();
    cfg
}
