use crate::error::VideoValidationError;
use crate::models::{VideoInfo, VideoValidationResult};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

/// Target video parameters for validation
const TARGET_FPS: f64 = 30.0;
const TARGET_HEIGHT: u32 = 1080;
const TARGET_WIDTH: u32 = 1920;
const TARGET_BITRATE_KBPS: u64 = 30_000;

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

        if (fps - TARGET_FPS).abs() > 1.0 {
            warnings.push(format!(
                "Frame rate {fps:.1}fps differs from target {TARGET_FPS}fps"
            ));
        }

        if height != TARGET_HEIGHT || width != TARGET_WIDTH {
            warnings.push(format!(
                "Resolution {width}x{height} differs from target {TARGET_WIDTH}x{TARGET_HEIGHT}"
            ));
        }

        if bitrate_kbps > 0 && (bitrate_kbps as i64 - TARGET_BITRATE_KBPS as i64).unsigned_abs() > 10_000 {
            warnings.push(format!(
                "Bitrate {bitrate_kbps}kbps differs from target {TARGET_BITRATE_KBPS}kbps"
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
