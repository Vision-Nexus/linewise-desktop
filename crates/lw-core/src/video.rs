use crate::error::VideoValidationError;
use crate::models::{VideoInfo, VideoValidationResult};
use std::path::Path;

/// Expected video parameters with tolerance ranges (advisory, not blocking)
const FPS_MIN: f64 = 20.0;
const FPS_MAX: f64 = 40.0;
const FPS_TARGET: f64 = 30.0;
const RESOLUTION_MIN_HEIGHT: u32 = 720;
const BITRATE_MIN_KBPS: u64 = 10_000;
const BITRATE_MAX_KBPS: u64 = 35_000;
const BITRATE_TARGET_KBPS: u64 = 30_000;

/// Link to guide users on how to change camera settings
pub const CAMERA_SETTINGS_GUIDE: &str = "https://docs.linewise.io/camera-settings";

/// Probe a video file using ffmpeg-next and validate against target parameters
pub async fn validate_video(path: &Path) -> Result<VideoValidationResult, VideoValidationError> {
    let path = path.to_path_buf();

    tokio::task::spawn_blocking(move || probe_and_validate(&path))
        .await
        .map_err(|e| VideoValidationError::ProbeFailed(e.to_string()))?
}

fn probe_and_validate(path: &Path) -> Result<VideoValidationResult, VideoValidationError> {
    let ictx = ffmpeg_next::format::input(path)
        .map_err(|e| VideoValidationError::ProbeFailed(format!("Failed to open: {e}")))?;

    // Find video stream
    let video_stream = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or_else(|| {
            VideoValidationError::UnsupportedFormat("No video stream found".to_string())
        })?;

    let video_params = video_stream.parameters();
    let video_ctx = ffmpeg_next::codec::context::Context::from_parameters(video_params)
        .map_err(|e| VideoValidationError::ProbeFailed(format!("Video codec context: {e}")))?;
    let video_dec = video_ctx
        .decoder()
        .video()
        .map_err(|e| VideoValidationError::ProbeFailed(format!("Video decoder: {e}")))?;

    let width = video_dec.width();
    let height = video_dec.height();
    let codec = video_stream.parameters().id().name().to_string();

    // Frame rate from stream
    let rate = video_stream.rate();
    let fps = if rate.1 > 0 {
        rate.0 as f64 / rate.1 as f64
    } else {
        0.0
    };

    // Bitrate: prefer decoder bit_rate, fallback to format-level
    let bitrate_bps = if video_dec.bit_rate() > 0 {
        video_dec.bit_rate() as u64
    } else {
        ictx.bit_rate().max(0) as u64
    };
    let bitrate_kbps = bitrate_bps / 1000;

    // Duration from format context (in seconds)
    let duration_secs = ictx.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE);

    // Format name
    let format = ictx.format().name().to_string();

    // Find audio stream codec
    let audio_codec = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Audio)
        .map(|s| s.parameters().id().name().to_string())
        .unwrap_or_default();

    let info = VideoInfo {
        width,
        height,
        fps,
        bitrate_kbps,
        codec,
        audio_codec,
        duration_secs,
        format,
    };

    let mut warnings = Vec::new();

    // FPS: acceptable range 20-40, target 30
    if fps > 0.0 && !(FPS_MIN..=FPS_MAX).contains(&fps) {
        warnings.push(format!(
            "Frame rate {fps:.1}fps is outside recommended range ({FPS_MIN:.0}-{FPS_MAX:.0}fps, target {FPS_TARGET:.0}fps)"
        ));
    }

    // Resolution: warn if below 720p
    if height > 0 && height < RESOLUTION_MIN_HEIGHT {
        warnings.push(format!(
            "Resolution {width}x{height} is below minimum recommended ({RESOLUTION_MIN_HEIGHT}p)"
        ));
    }

    // Bitrate: acceptable range 10M-35M
    if bitrate_kbps > 0 && !(BITRATE_MIN_KBPS..=BITRATE_MAX_KBPS).contains(&bitrate_kbps) {
        warnings.push(format!(
            "Bitrate {bitrate_kbps}kbps is outside recommended range ({}-{}Mbps, target {}Mbps)",
            BITRATE_MIN_KBPS / 1000,
            BITRATE_MAX_KBPS / 1000,
            BITRATE_TARGET_KBPS / 1000,
        ));
    }

    if !warnings.is_empty() {
        warnings.push(format!(
            "See how to adjust your camera settings: {CAMERA_SETTINGS_GUIDE}"
        ));
    }

    Ok(VideoValidationResult { info, warnings })
}
