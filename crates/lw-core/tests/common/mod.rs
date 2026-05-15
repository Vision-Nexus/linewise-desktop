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
use ffmpeg_next::{Rational, codec, format, picture};
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
/// We delegate to the `ffmpeg` CLI (already a build prerequisite for
/// transcode tests, see `Cargo.toml` ffmpeg-next dep) instead of
/// hand-rolling the encoder. Hand-rolled fixtures kept breaking across
/// libavcodec versions: Ubuntu 22.04's FFmpeg 4.4 wanted FLTP planar
/// f32 frames with `nb_samples == frame_size` exactly, FFmpeg 7 on
/// Homebrew was lenient, FFmpeg 8 (which landed on the macos-14 runner
/// image in May 2026) tightened up frame validation again and started
/// returning EINVAL from `send_frame` for the same fixture code that
/// worked the previous month. The CLI handles every version-specific
/// quirk for us; the test only cares that the resulting file has an
/// audio stream and no video stream.
pub fn synthesize_audio_only_fixture(path: &Path) {
    let path_str = path
        .to_str()
        .expect("audio fixture path must be valid UTF-8");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
            "-t",
            "1",
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            path_str,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn ffmpeg CLI");
    assert!(
        status.success(),
        "ffmpeg CLI failed to write audio-only fixture (exit: {status})"
    );
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
        metadata: Vec::new(),
        telemetry: None,
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
        metadata: Vec::new(),
        telemetry: None,
    }
}

pub fn forced_software_config() -> TranscodeConfig {
    let mut cfg = TranscodeConfig::default();
    cfg.hw_accel = "none".into();
    cfg.preset = "ultrafast".into();
    cfg
}
