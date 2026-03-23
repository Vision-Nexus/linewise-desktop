use crate::error::VideoValidationError;
use crate::models::{VideoInfo, VideoValidationResult};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

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

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    streams: Vec<FfprobeStream>,
    format: FfprobeFormat,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: String,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    format_name: String,
    duration: Option<String>,
    bit_rate: Option<String>,
}

/// Probe a video file using ffprobe and validate against target parameters
pub async fn validate_video(path: &Path) -> Result<VideoValidationResult, VideoValidationError> {
    let path = path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_streams",
                "-show_format",
            ])
            .arg(&path)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    VideoValidationError::FfprobeNotFound
                } else {
                    VideoValidationError::ProbeFailed(e.to_string())
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VideoValidationError::ProbeFailed(stderr.to_string()));
        }

        let probe: FfprobeOutput = serde_json::from_slice(&output.stdout)
            .map_err(|e| VideoValidationError::ProbeFailed(e.to_string()))?;

        let video_stream = probe
            .streams
            .iter()
            .find(|s| s.codec_type == "video")
            .ok_or_else(|| {
                VideoValidationError::UnsupportedFormat("No video stream found".to_string())
            })?;

        let width = video_stream.width.unwrap_or(0);
        let height = video_stream.height.unwrap_or(0);
        let codec = video_stream
            .codec_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Parse frame rate from "30/1" or "30000/1001" format
        let fps = video_stream
            .r_frame_rate
            .as_deref()
            .and_then(parse_frame_rate)
            .unwrap_or(0.0);

        // Bitrate: prefer stream bitrate, fallback to format bitrate
        let bitrate_str = video_stream
            .bit_rate
            .as_deref()
            .or(probe.format.bit_rate.as_deref())
            .unwrap_or("0");
        let bitrate_kbps = bitrate_str.parse::<u64>().unwrap_or(0) / 1000;

        let duration_secs = probe
            .format
            .duration
            .as_deref()
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(0.0);

        let info = VideoInfo {
            width,
            height,
            fps,
            bitrate_kbps,
            codec,
            duration_secs,
            format: probe.format.format_name,
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
    })
    .await
    .map_err(|e| VideoValidationError::ProbeFailed(e.to_string()))?
}

fn parse_frame_rate(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split('/').collect();
    match parts.as_slice() {
        [num, den] => {
            let n: f64 = num.parse().ok()?;
            let d: f64 = den.parse().ok()?;
            if d == 0.0 {
                None
            } else {
                Some(n / d)
            }
        }
        [num] => num.parse().ok(),
        _ => None,
    }
}
