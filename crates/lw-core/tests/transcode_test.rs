//! Real-world transcode smoke test.
//!
//! Runs an actual ffmpeg-next transcode of a small public sample video to
//! prove the end-to-end pipeline works on a non-synthetic input. The
//! sample is downloaded from Google's Shaka Player demo bucket on first
//! run and cached under `/tmp/linewise-test-videos/`, so subsequent
//! runs are offline.
//!
//! When no internet is available and the cache is empty, the test prints
//! a skip message and exits — network flakiness is a genuine environment
//! concern, unlike FFmpeg which we treat as a build prerequisite.

use lw_core::config::TranscodeConfig;
use lw_core::transcode;
use lw_core::video;
use std::path::{Path, PathBuf};

/// ~2.7 MB h264/mp4 sample, 5 seconds. samplelib.com publishes a stable
/// set of MP4 fixtures at predictable URLs; if the host ever rotates,
/// swap to another `sample-{5s,10s,15s,20s,30s}.mp4` slug.
const SAMPLE_URL: &str = "https://samplelib.com/lib/preview/mp4/sample-5s.mp4";
const SAMPLE_FILENAME: &str = "sample-5s.mp4";

async fn ensure_sample_video() -> Option<PathBuf> {
    let cache_dir = std::env::temp_dir().join("linewise-test-videos");
    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        eprintln!("could not create cache dir {}: {e}", cache_dir.display());
        return None;
    }
    let cache_path = cache_dir.join(SAMPLE_FILENAME);

    let cached_size = std::fs::metadata(&cache_path).map(|m| m.len()).unwrap_or(0);
    if cached_size > 0 {
        return Some(cache_path);
    }

    eprintln!(
        "downloading sample video {SAMPLE_URL} → {}",
        cache_path.display()
    );
    let client = match reqwest::Client::builder()
        .user_agent("linewise-desktop-tests/1.0")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("could not build http client: {e} — skipping");
            return None;
        }
    };
    let resp = match client.get(SAMPLE_URL).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("download failed (network unreachable?): {e} — skipping");
            return None;
        }
    };
    if !resp.status().is_success() {
        eprintln!("download returned HTTP {} — skipping", resp.status());
        return None;
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read body failed: {e} — skipping");
            return None;
        }
    };
    if let Err(e) = std::fs::write(&cache_path, &bytes) {
        eprintln!("could not write cache file: {e} — skipping");
        return None;
    }
    Some(cache_path)
}

#[tokio::test]
async fn test_transcode_real_file() {
    let Some(path) = ensure_sample_video().await else {
        return;
    };

    ffmpeg_next::init().expect("ffmpeg init");
    let path: &Path = &path;

    let result = video::validate_video(path).await.expect("probe failed");
    let info = &result.info;
    eprintln!(
        "Input: {} {}x{} {:.0}fps {}kbps {:.1}s",
        info.codec, info.width, info.height, info.fps, info.bitrate_kbps, info.duration_secs
    );

    let config = TranscodeConfig::default();
    eprintln!(
        "Config: crf={} preset={} max={}Mbps",
        config.crf, config.preset, config.max_bitrate_mbps
    );

    let tc_result = transcode::transcode_video(path, info, &config, &|done, total| {
        if done % 100 == 0 || done == total {
            eprintln!("  {done}/{total} frames");
        }
    });

    match tc_result {
        Ok(r) => {
            eprintln!(
                "OK: {:.1}MB → {:.1}MB at {}",
                r.original_size as f64 / 1_048_576.0,
                r.transcoded_size as f64 / 1_048_576.0,
                r.output_path.display()
            );
            let _ = std::fs::remove_file(&r.output_path);
        }
        Err(e) => panic!("Transcode failed: {e}"),
    }
}
