//! Display helpers for the upload-queue popover. After the server-side
//! quality-check cutover, all rule evaluation lives on the API; this
//! module only owns the small pure helpers the UI uses to render the
//! `VideoInfo` it gets back from the server.

use crate::config::TranscodeConfig;
use crate::models::VideoInfo;

/// Render a bitrate as a short, human-readable string. ≥1000 kbps becomes
/// `54.3Mbps` (one decimal); below 1000 stays in `kbps`. Used for both the
/// popover summary line and the validation warnings, so the units the user
/// sees match across surfaces.
pub fn format_bitrate(kbps: u64) -> String {
    if kbps >= 1000 {
        format!("{:.1}Mbps", kbps as f64 / 1000.0)
    } else {
        format!("{kbps}kbps")
    }
}

/// Vendor signature used by [`device_info_rows`] to split an encoder
/// string like `"DJI Osmo Nano"` into Make + Model when the file has
/// no explicit `make` / `model` keys. Purely a UI affordance — the
/// server-side rule engine has its own classifier, so this list does
/// not have to be exhaustive or kept in sync with the server's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEncoderSignature {
    /// Lowercase substring matched against the encoder string.
    pub needle: &'static str,
    /// Vendor label rendered as the Make.
    pub vendor_label: &'static str,
}

/// Hardcoded vendor signature list. Mirrors the camera vendors whose
/// firmware writes the device name into the encoder tag rather than
/// into a `make` key. Kept tiny on purpose — anything more elaborate
/// belongs on the server.
const DEVICE_ENCODER_SIGNATURES: &[DeviceEncoderSignature] = &[
    DeviceEncoderSignature {
        needle: "dji",
        vendor_label: "DJI",
    },
    DeviceEncoderSignature {
        needle: "gopro",
        vendor_label: "GoPro",
    },
    DeviceEncoderSignature {
        needle: "insta360",
        vendor_label: "Insta360",
    },
];

/// Read-only handle to the vendor signature list. Returned as a slice
/// so callers can pass it straight into [`device_info_rows`].
pub fn device_encoder_signatures() -> &'static [DeviceEncoderSignature] {
    DEVICE_ENCODER_SIGNATURES
}

/// Normalize the device-info group into labelled rows. Multiple raw keys
/// fold into a single line each: `make` and any `*.make` variant become
/// "Make"; same for `model` and `software`. Returns an empty string in
/// the value when nothing was found, so the UI can render the absence
/// instead of hiding the row — that's the whole point of showing device
/// info on REJECTED clips.
///
/// When the file has no `make` / `model` keys but the encoder string
/// names a known camera vendor (DJI / GoPro / etc.), we split the
/// encoder tag into a vendor + remainder pair so the device row reads
/// "DJI / Osmo Nano" instead of leaving Make and Model empty.
pub fn device_info_rows(
    info: &VideoInfo,
    signatures: &[DeviceEncoderSignature],
) -> Vec<(&'static str, String)> {
    let lookup = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| {
            info.metadata
                .iter()
                .find_map(|(mk, mv)| (mk == k).then(|| mv.clone()))
        })
    };

    let make_keys = &[
        "make",
        "com.apple.quicktime.make",
        "com.android.manufacturer",
        "device_manufacturer",
    ];
    let model_keys = &[
        "model",
        "com.apple.quicktime.model",
        "com.android.model",
        "device_model",
    ];

    let mut make = lookup(make_keys).unwrap_or_default();
    let mut model = lookup(model_keys).unwrap_or_default();
    let software = lookup(&["com.apple.quicktime.software", "encoder"]).unwrap_or_default();
    let recorded = lookup(&["creation_time"]).unwrap_or_default();

    // Vendor fallback: if make/model are empty but the encoder string
    // names a known vendor, split it. "DJI Osmo Nano" → make=DJI,
    // model=Osmo Nano. Pure encoder strings like "HEVC" don't match
    // and leave the rows empty.
    if make.is_empty() && model.is_empty() && !software.is_empty() {
        let needle = software.trim().to_ascii_lowercase();
        if let Some(sig) = signatures.iter().find(|sig| needle.contains(sig.needle)) {
            make = sig.vendor_label.to_string();
            // Strip vendor tokens from the encoder string; what's
            // left is the model. If nothing's left (encoder was just
            // the vendor name), fall back to the full encoder so the
            // row shows something.
            let remainder = software
                .split_whitespace()
                .filter(|w| !w.eq_ignore_ascii_case(sig.vendor_label))
                .collect::<Vec<_>>()
                .join(" ");
            model = if remainder.is_empty() {
                software.clone()
            } else {
                remainder
            };
        }
    }

    vec![
        ("Make", make),
        ("Model", model),
        ("Software", software),
        ("Recorded", recorded),
    ]
}

/// Would transcoding actually shrink this clip? Returns false when the
/// source is at or below the target on resolution, fps, and bitrate —
/// in that case a transcode only costs CPU/storage without adding
/// value. The UI uses this to hide the per-clip transcode toggle;
/// `upload::maybe_transcode` also short-circuits on `false` as
/// defense-in-depth.
pub fn transcode_would_help(info: &VideoInfo, cfg: &TranscodeConfig) -> bool {
    let resolution_exceeds = info.height > cfg.max_height;
    let fps_exceeds = cfg.target_fps > 0 && info.fps > cfg.target_fps as f64;
    let bitrate_exceeds = info.bitrate_kbps > (cfg.max_bitrate_mbps as u64) * 1000;
    resolution_exceeds || fps_exceeds || bitrate_exceeds
}

#[cfg(test)]
mod tests {
    use super::*;

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
            metadata: Vec::new(),
            telemetry: None,
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

    fn full_info(width: u32, height: u32, fps: f64, bitrate_kbps: u64) -> VideoInfo {
        VideoInfo {
            width,
            height,
            fps,
            bitrate_kbps,
            codec: "h264".into(),
            audio_codec: "aac".into(),
            duration_secs: 60.0,
            format: "mp4".into(),
            metadata: Vec::new(),
            telemetry: None,
        }
    }

    #[test]
    fn device_info_splits_dji_encoder() {
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata
            .push(("encoder".into(), "DJI Osmo Nano".into()));
        let rows = device_info_rows(&info, device_encoder_signatures());
        let make = rows
            .iter()
            .find(|(k, _)| *k == "Make")
            .expect("Make row")
            .1
            .clone();
        let model = rows
            .iter()
            .find(|(k, _)| *k == "Model")
            .expect("Model row")
            .1
            .clone();
        assert_eq!(make, "DJI");
        assert_eq!(model, "Osmo Nano");
    }

    #[test]
    fn device_info_keeps_existing_make_when_present() {
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata
            .push(("com.apple.quicktime.make".into(), "Apple".into()));
        info.metadata
            .push(("com.apple.quicktime.model".into(), "iPhone 13 Pro".into()));
        info.metadata
            .push(("com.apple.quicktime.software".into(), "15.0.1".into()));
        let rows = device_info_rows(&info, device_encoder_signatures());
        let make = rows
            .iter()
            .find(|(k, _)| *k == "Make")
            .expect("Make row")
            .1
            .clone();
        let model = rows
            .iter()
            .find(|(k, _)| *k == "Model")
            .expect("Model row")
            .1
            .clone();
        assert_eq!(make, "Apple");
        assert_eq!(model, "iPhone 13 Pro");
    }

    #[test]
    fn device_info_recognizes_gopro_encoder() {
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata
            .push(("encoder".into(), "GoPro HERO12".into()));
        let rows = device_info_rows(&info, device_encoder_signatures());
        let make = rows
            .iter()
            .find(|(k, _)| *k == "Make")
            .expect("Make row")
            .1
            .clone();
        let model = rows
            .iter()
            .find(|(k, _)| *k == "Model")
            .expect("Model row")
            .1
            .clone();
        assert_eq!(make, "GoPro");
        assert_eq!(model, "HERO12");
    }

    #[test]
    fn device_info_leaves_unknown_encoder_alone() {
        let mut info = full_info(2560, 1440, 30.0, 15_000);
        info.metadata.push(("encoder".into(), "HEVC".into()));
        let rows = device_info_rows(&info, device_encoder_signatures());
        let make = rows
            .iter()
            .find(|(k, _)| *k == "Make")
            .expect("Make row")
            .1
            .clone();
        let model = rows
            .iter()
            .find(|(k, _)| *k == "Model")
            .expect("Model row")
            .1
            .clone();
        assert!(make.is_empty(), "expected empty Make, got {make:?}");
        assert!(model.is_empty(), "expected empty Model, got {model:?}");
    }
}
