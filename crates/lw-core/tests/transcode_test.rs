use lw_core::config::TranscodeConfig;
use lw_core::transcode;
use lw_core::video;
use std::path::Path;

#[tokio::test]
async fn test_transcode_real_file() {
    let path = Path::new("/Users/famer.me/Downloads/ae909add-19fd-4401-b2af-5384e24eedfb.mp4");
    if !path.exists() {
        eprintln!("Test file not found, skipping");
        return;
    }

    ffmpeg_next::init().expect("ffmpeg init");

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
