//! Integration tests for the resumable transcode pipeline.
//!
//! Test 3: an audio-only input fails cleanly with "No video stream".
//! Test 4: full encode → simulated crash → resume → final MP4 has the
//!         right duration and the resume path actually executed.
//!
//! Both tests synthesize their fixtures at runtime — no checked-in binaries.
//! FFmpeg with libx264, libx265, and AAC are build prerequisites of `lw-core`;
//! missing them is a build-environment defect, not a skip condition.

mod common;

use common::{
    FIXTURE_DURATION_SECS, FIXTURE_FPS, audio_only_video_info, ffmpeg_init, forced_software_config,
    fresh_temp_dir, probe_duration_secs, synthesize_audio_only_fixture, synthesize_video_fixture,
    synthetic_video_info,
};
use lw_core::error::TranscodeError;
use lw_core::transcode;
use std::path::Path;

#[test]
fn audio_only_input_fails_cleanly() {
    ffmpeg_init();
    let dir = fresh_temp_dir("audio-only");
    let input = dir.join("audio-only.m4a");
    synthesize_audio_only_fixture(&input);

    let info = audio_only_video_info(1.0);
    let cfg = forced_software_config();

    let result = transcode::transcode_video(&input, &info, &cfg, &|_, _| {});

    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Err(TranscodeError::EncodingFailed(msg)) => {
            assert!(
                msg.contains("No video stream"),
                "expected 'No video stream' in error, got: {msg}"
            );
        }
        other => panic!("expected EncodingFailed(No video stream), got {other:?}"),
    }
}

#[test]
fn happy_path_full_transcode_matches_input_duration() {
    ffmpeg_init();
    let dir = fresh_temp_dir("happy-path");
    // Per-test input stem keeps `prepare_paths` scratch dirs distinct so
    // tests can run in parallel without colliding on /tmp/linewise-transcode/<stem>_hls.
    let input = dir.join(format!("happy-{}.mp4", uuid::Uuid::new_v4()));
    let total_frames = (FIXTURE_DURATION_SECS * FIXTURE_FPS as f64) as u64;
    synthesize_video_fixture(&input, total_frames);

    let info = synthetic_video_info(FIXTURE_DURATION_SECS);
    let cfg = forced_software_config();

    let result = transcode::transcode_video(&input, &info, &cfg, &|_, _| {});

    match result {
        Ok(r) => {
            assert!(r.output_path.exists(), "output mp4 should exist");
            assert!(r.transcoded_size > 0, "output mp4 should be non-empty");
            let out_dur = probe_duration_secs(&r.output_path);
            assert!(
                (out_dur - FIXTURE_DURATION_SECS).abs() < 1.0,
                "output duration {out_dur:.2}s drifted from input {FIXTURE_DURATION_SECS:.2}s by more than 1s"
            );
            // Scratch directory should be cleaned up after success.
            let (scratch, _) = transcode::prepare_paths(&input).expect("prepare_paths");
            assert!(
                !scratch.exists(),
                "scratch dir {} should be cleaned up after success",
                scratch.display()
            );
            let _ = std::fs::remove_file(&r.output_path);
        }
        Err(e) => panic!("happy-path transcode failed: {e}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resume_from_partial_scratch_dir_recovers_full_duration() {
    ffmpeg_init();
    let dir = fresh_temp_dir("resume");
    let input = dir.join(format!("resume-{}.mp4", uuid::Uuid::new_v4()));
    let total_frames = (FIXTURE_DURATION_SECS * FIXTURE_FPS as f64) as u64;
    synthesize_video_fixture(&input, total_frames);

    let info = synthetic_video_info(FIXTURE_DURATION_SECS);
    let cfg = forced_software_config();

    // Stage 1: do a full encode to HLS into a known scratch dir, *without*
    // running the cleanup wrapper, so we can corrupt the tail and re-run.
    let (scratch, output) = transcode::prepare_paths(&input).expect("prepare_paths");
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_file(&output);
    std::fs::create_dir_all(&scratch).expect("mkdir scratch");

    let initial_resume = transcode::ResumePoint {
        start_seg: 0,
        resume_seconds: 0.0,
    };
    transcode::encode_to_hls(&input, &scratch, &info, &cfg, &initial_resume, &|_, _| {})
        .expect("seed encode");

    let mut seg_count = count_segments(&scratch);
    assert!(
        seg_count >= 2,
        "seed encode should produce at least 2 segments, got {seg_count}"
    );

    // Stage 2: simulate a crash with a garbage tail segment.
    let garbage_index = seg_count;
    let garbage_path = scratch.join(format!("media-hd{garbage_index:010}.ts"));
    std::fs::write(&garbage_path, vec![0u8; 200]).expect("write garbage seg");
    seg_count += 1;
    assert_eq!(count_segments(&scratch), seg_count);

    // Stage 3: call the public entry point. detect_resume_point must drop
    // the garbage tail, encode_to_hls must resume, concat_to_mp4 must
    // produce a full-duration MP4, scratch must be cleaned up.
    let result = transcode::transcode_video(&input, &info, &cfg, &|_, _| {});

    match result {
        Ok(r) => {
            assert!(
                !garbage_path.exists(),
                "garbage tail segment must be dropped"
            );
            assert!(r.output_path.exists(), "resumed mp4 should exist");
            let out_dur = probe_duration_secs(&r.output_path);
            assert!(
                (out_dur - FIXTURE_DURATION_SECS).abs() < 1.5,
                "resumed output duration {out_dur:.2}s drifted from input {FIXTURE_DURATION_SECS:.2}s by more than 1.5s — resume probably re-encoded from zero or duplicated segments"
            );
            let _ = std::fs::remove_file(&r.output_path);
        }
        Err(e) => panic!("resume transcode failed: {e}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

fn count_segments(scratch_dir: &Path) -> u64 {
    std::fs::read_dir(scratch_dir)
        .expect("read scratch")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("media-hd") && n.ends_with(".ts"))
                .unwrap_or(false)
        })
        .count() as u64
}
