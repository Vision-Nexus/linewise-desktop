//! HW-encoder smoke test for Linux + Windows (NVENC / AMF / QSV).
//!
//! Probes for any registered + openable HW family via the same path the
//! production code uses (`find_first_available`). If no HW driver is
//! installed on this machine, the test skips with `eprintln!`. Otherwise
//! it forces that family via `hw_accel`, transcodes the synthetic clip,
//! and asserts the output MP4's first video stream carries `hvc1`.
//!
//! The whole file is gated to non-macOS desktop targets; on macOS it
//! compiles to an empty translation unit (test 5a covers VideoToolbox).

#![cfg(any(target_os = "linux", target_os = "windows"))]
#![allow(clippy::field_reassign_with_default)]

mod common;

use common::{
    FIXTURE_DURATION_SECS, FIXTURE_FPS, ffmpeg_init, fresh_temp_dir, probe_duration_secs,
    read_video_codec_tag, synthesize_video_fixture, synthetic_video_info,
};
use lw_core::config::TranscodeConfig;
use lw_core::transcode::{self, EncoderKind};

fn hw_accel_config(family: &str) -> TranscodeConfig {
    let mut cfg = TranscodeConfig::default();
    cfg.codec = "hevc".into();
    cfg.hw_accel = family.into();
    cfg
}

fn family_label(kind: EncoderKind) -> &'static str {
    match kind {
        EncoderKind::Nvenc => "nvenc",
        EncoderKind::Qsv => "qsv",
        EncoderKind::Amf => "amf",
        EncoderKind::Software | EncoderKind::VideoToolbox => "(unexpected on this platform)",
    }
}

#[test]
fn hw_transcode_writes_hvc1_codec_tag_when_driver_present() {
    ffmpeg_init();
    let candidates = EncoderKind::hw_candidates("hevc");
    let Some((name, kind)) = transcode::find_first_available(candidates) else {
        eprintln!(
            "no HEVC hardware encoder probe-opens on this machine — skipping. \
             Install NVIDIA / Intel / AMD drivers + matching ffmpeg build to exercise this test."
        );
        return;
    };
    let family = family_label(kind);
    eprintln!("HW probe selected encoder {name} (family {family})");

    let dir = fresh_temp_dir("hw-other-hvc1");
    let input = dir.join(format!("hw-{}.mp4", uuid::Uuid::new_v4()));
    let total_frames = (FIXTURE_DURATION_SECS * FIXTURE_FPS as f64) as u64;
    synthesize_video_fixture(&input, total_frames);

    let info = synthetic_video_info(FIXTURE_DURATION_SECS);
    let cfg = hw_accel_config(family);

    let started = std::time::Instant::now();
    let result = transcode::transcode_video(&input, &info, &cfg, &|_, _| {});
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "HW transcode of a 12s synthetic clip took {elapsed:?} — something is wrong"
    );

    match result {
        Ok(r) => {
            assert!(r.output_path.exists(), "HW output mp4 should exist");
            let tag = read_video_codec_tag(&r.output_path);
            assert_eq!(
                &tag, b"hvc1",
                "expected hvc1 codec_tag from {name}, got bytes {tag:?}"
            );
            let dur = probe_duration_secs(&r.output_path);
            assert!(
                (dur - FIXTURE_DURATION_SECS).abs() < 1.5,
                "HW ({name}) output duration {dur:.2}s drifted from input {FIXTURE_DURATION_SECS:.2}s by more than 1.5s"
            );
            let _ = std::fs::remove_file(&r.output_path);
        }
        Err(e) => panic!("HW ({name}) transcode failed: {e}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
