use crate::config::TranscodeConfig;
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
    // Treat the three structural failure points below as `Unplayable`. They
    // fire when the container has no usable timeline — most often a missing
    // `moov` atom from a power-cut MP4/MOV recording, but also truncated
    // headers and unreadable codec parameters. The libav error string is
    // already informative ("Invalid data found when processing input" for
    // the moov case), so we keep it in the reason field.
    let ictx = ffmpeg_next::format::input(path).map_err(|e| VideoValidationError::Unplayable {
        reason: format!(
            "container could not be opened (likely missing moov atom or truncated): {e}"
        ),
    })?;

    let video_stream = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or_else(|| VideoValidationError::Unplayable {
            reason: "no video stream in container".to_string(),
        })?;

    let video_params = video_stream.parameters();
    let video_ctx =
        ffmpeg_next::codec::context::Context::from_parameters(video_params).map_err(|e| {
            VideoValidationError::Unplayable {
                reason: format!("video stream codec parameters unreadable: {e}"),
            }
        })?;
    let video_dec = video_ctx
        .decoder()
        .video()
        .map_err(|e| VideoValidationError::Unplayable {
            reason: format!("video decoder could not be initialised: {e}"),
        })?;

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

/// Would transcoding actually shrink this clip? Returns false when the source
/// is at or below the target on resolution, fps, and bitrate — in that case
/// a transcode only costs CPU/storage without adding value. The UI uses this
/// to hide the per-clip transcode toggle; `upload::maybe_transcode` also
/// short-circuits on `false` as defense-in-depth.
pub fn transcode_would_help(info: &VideoInfo, cfg: &TranscodeConfig) -> bool {
    let resolution_exceeds = info.height > cfg.max_height;
    let fps_exceeds = cfg.target_fps > 0 && info.fps > cfg.target_fps as f64;
    let bitrate_exceeds = info.bitrate_kbps > (cfg.max_bitrate_mbps as u64) * 1000;
    resolution_exceeds || fps_exceeds || bitrate_exceeds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a minimal ISO-BMFF file containing only an `ftyp` box — no
    /// `moov`, no `mdat`. This is the shape of a power-cut MP4/MOV: the
    /// header arrives, but the trailing `moov` (which holds the index)
    /// never gets written. ffmpeg's `mov,mp4,m4a,3gp` demuxer rejects it
    /// with "moov atom not found / Invalid data found when processing
    /// input", which is exactly the case we want to detect.
    fn write_moovless_mp4(path: &std::path::Path) {
        // Box layout (big-endian):
        //   [size:u32][type:'ftyp'][major:'isom'][minor:0][compat:'isom','avc1']
        // Total: 24 bytes.
        let bytes: &[u8] = &[
            0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0x00, 0x00,
            0x02, 0x00, b'i', b's', b'o', b'm', b'a', b'v', b'c', b'1',
        ];
        std::fs::write(path, bytes).expect("write fixture");
    }

    #[tokio::test]
    async fn unplayable_when_moov_missing() {
        let path =
            std::env::temp_dir().join(format!("lw-test-no-moov-{}.mp4", uuid::Uuid::new_v4()));
        write_moovless_mp4(&path);

        let result = validate_video(&path).await;

        let _ = std::fs::remove_file(&path);

        match result {
            Err(VideoValidationError::Unplayable { reason }) => {
                assert!(
                    !reason.is_empty(),
                    "Unplayable reason should carry the libav error string, got empty"
                );
            }
            other => panic!("expected Unplayable, got {other:?}"),
        }
    }

    fn info(height: u32, fps: f64, bitrate_kbps: u64) -> VideoInfo {
        VideoInfo {
            width: 1920,
            height,
            fps,
            bitrate_kbps,
            codec: "h264".into(),
            audio_codec: "aac".into(),
            duration_secs: 60.0,
            format: "mov".into(),
        }
    }

    fn cfg() -> TranscodeConfig {
        TranscodeConfig {
            enabled: true,
            codec: "hevc".into(),
            crf: 23,
            preset: "medium".into(),
            target_bitrate_mbps: 10,
            max_bitrate_mbps: 20,
            max_height: 1080,
            audio_bitrate_kbps: 128,
            target_fps: 30,
            hw_accel: "auto".into(),
        }
    }

    #[test]
    fn guard_exceeds_resolution() {
        assert!(transcode_would_help(&info(1440, 30.0, 8_000), &cfg()));
    }

    #[test]
    fn guard_exceeds_fps() {
        assert!(transcode_would_help(&info(1080, 60.0, 8_000), &cfg()));
    }

    #[test]
    fn guard_exceeds_bitrate() {
        assert!(transcode_would_help(&info(1080, 30.0, 25_000), &cfg()));
    }

    #[test]
    fn guard_source_below_all() {
        // 720p 30fps 4Mbps — transcoding is pure waste.
        assert!(!transcode_would_help(&info(720, 30.0, 4_000), &cfg()));
    }

    #[test]
    fn guard_fps_ignored_when_target_zero() {
        let mut c = cfg();
        c.target_fps = 0;
        // 60fps at 720p 4Mbps with no target_fps → no axis exceeds.
        assert!(!transcode_would_help(&info(720, 60.0, 4_000), &c));
    }
}
