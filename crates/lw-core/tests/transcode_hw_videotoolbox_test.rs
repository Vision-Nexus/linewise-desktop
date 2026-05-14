//! VideoToolbox-specific HW encoder smoke test (macOS only).
//!
//! Verifies that on macOS:
//!   1. `resolve_encoder` with `hw_accel = "videotoolbox"` returns the
//!      VideoToolbox family.
//!   2. A real transcode through that family produces an MP4 whose first
//!      video stream carries the `hvc1` codec_tag (not `hev1`). Without the
//!      explicit `hvc1` write, VideoToolbox emits `hev1`, which Safari
//!      tolerates but most browser `<video>` players reject.
//!
//! The whole file is `#![cfg(target_os = "macos")]` — on Linux/Windows it
//! compiles to an empty translation unit.

#![cfg(target_os = "macos")]
#![allow(clippy::field_reassign_with_default)]

mod common;

use common::{
    FIXTURE_DURATION_SECS, FIXTURE_FPS, ffmpeg_init, fresh_temp_dir, probe_duration_secs,
    read_video_codec_tag, synthesize_video_fixture, synthetic_video_info,
};
use lw_core::config::TranscodeConfig;
use lw_core::transcode::{self, EncoderKind};

fn videotoolbox_config() -> TranscodeConfig {
    let mut cfg = TranscodeConfig::default();
    cfg.codec = "hevc".into();
    cfg.hw_accel = "videotoolbox".into();
    cfg
}

#[test]
fn resolve_encoder_picks_videotoolbox() {
    ffmpeg_init();
    let cfg = videotoolbox_config();
    let (name, kind) =
        transcode::resolve_encoder(&cfg).expect("VideoToolbox should resolve on macOS");
    assert_eq!(name, "hevc_videotoolbox");
    assert_eq!(kind, EncoderKind::VideoToolbox);
}

#[test]
fn videotoolbox_transcode_writes_hvc1_codec_tag() {
    ffmpeg_init();
    let dir = fresh_temp_dir("vt-hvc1");
    let input = dir.join(format!("vt-{}.mp4", uuid::Uuid::new_v4()));
    let total_frames = (FIXTURE_DURATION_SECS * FIXTURE_FPS as f64) as u64;
    synthesize_video_fixture(&input, total_frames);

    let info = synthetic_video_info(FIXTURE_DURATION_SECS);
    let cfg = videotoolbox_config();

    let result = transcode::transcode_video(&input, &info, &cfg, &|_, _| {});

    match result {
        Ok(r) => {
            assert!(r.output_path.exists(), "VT output mp4 should exist");
            let tag = read_video_codec_tag(&r.output_path);
            assert_eq!(
                &tag,
                b"hvc1",
                "expected hvc1 codec_tag, got {:?} (raw bytes {tag:?})",
                std::str::from_utf8(&tag).unwrap_or("?")
            );
            let dur = probe_duration_secs(&r.output_path);
            assert!(
                (dur - FIXTURE_DURATION_SECS).abs() < 1.5,
                "VT output duration {dur:.2}s drifted from input {FIXTURE_DURATION_SECS:.2}s by more than 1.5s"
            );
            let _ = std::fs::remove_file(&r.output_path);
        }
        Err(e) => panic!("VideoToolbox transcode failed: {e}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
