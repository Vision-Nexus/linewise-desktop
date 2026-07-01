use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub upload: UploadConfig,
    #[serde(default)]
    pub transcode: TranscodeConfig,
    pub camera: CameraConfig,
    pub app: GeneralConfig,
    #[serde(default)]
    pub watch_folders: Vec<WatchFolderEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_environment")]
    pub environment: Environment,
    /// Optional fixed HTTP proxy for every outbound client (API, auth,
    /// GCS uploads). When set to a non-empty `http://host:port` URL the
    /// three reqwest clients route through it instead of relying on the
    /// volatile Windows system-proxy snapshot captured at startup. Point
    /// it at v2ray's local HTTP inbound (e.g. `http://127.0.0.1:10809`):
    /// that endpoint is stable across GLOBAL↔RULE mode switches, so the
    /// uploaders' retry loops recover instead of wedging until restart.
    ///
    /// Empty / absent = previous behaviour (no explicit proxy). SOCKS is
    /// not supported (reqwest is built without the `socks` feature); an
    /// invalid value is logged and ignored, never panics. `#[serde(default)]`
    /// keeps config.toml files written before this field existed loading
    /// cleanly, so upgrading users need no migration.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Dev,
    Testing,
    Production,
}

impl Environment {
    pub fn api_base_url(&self) -> &'static str {
        match self {
            Self::Dev => "https://api.dev.linewise.io",
            Self::Testing => "https://api.testing.linewise.io",
            Self::Production => "https://api.product.linewise.io",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Dev => "Dev",
            Self::Testing => "Testing",
            Self::Production => "Production",
        }
    }
}

/// Release builds ship pointing at production; debug builds default to
/// dev so contributors don't accidentally talk to the production
/// backend. The persisted config.toml overrides both — system admins
/// flip this via the in-app environment switcher.
fn default_environment() -> Environment {
    if cfg!(debug_assertions) {
        Environment::Dev
    } else {
        Environment::Production
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadConfig {
    #[serde(default = "default_true")]
    pub auto_clean: bool,
    #[serde(default)]
    pub bandwidth_limit_mbps: u32,
    #[serde(default = "default_concurrent")]
    pub max_concurrent_uploads: u32,
    /// Floor (in whole MiB) for the dynamic resumable chunk size. The uploader
    /// scales the chunk with the file size (see `storage::pick_chunk_size`); this
    /// raises the lower bound, and an explicit value above the auto cap overrides
    /// it. It is no longer a fixed chunk size.
    #[serde(default = "default_chunk_size")]
    pub chunk_size_mb: u32,
    /// Inert back-compat field. The sequential ("upload one by one") dispatch
    /// mode was removed in favour of bounded-parallel-only dispatch, but the
    /// key is kept so a config.toml written by an older build still
    /// deserializes cleanly. It no longer controls anything — nothing reads it
    /// for dispatch. `#[serde(default)]` also lets configs predating the field
    /// load without a migration.
    #[serde(default)]
    pub sequential_uploads: bool,
    /// How many parts of a single multipart (XML MPU) upload run concurrently.
    /// Six keeps a multi-GB upload saturating a fast link without fanning out to
    /// one TCP connection per part (which would thrash a slow/metered link and
    /// balloon peak RAM to `parts_in_flight * part_size`). Weak-network users
    /// (CN/HK) can lower it to 1–2 from the Network settings pane so a flaky link
    /// isn't overwhelmed by parallel PUTs. `#[serde(default)]` keeps older
    /// config.toml files loading without a migration.
    #[serde(default = "default_mpu_concurrency")]
    pub mpu_part_concurrency: u32,
}

fn default_true() -> bool {
    true
}
fn default_concurrent() -> u32 {
    3
}
/// Floor (in MiB) for the dynamic chunk sizer; 8 keeps small files efficient
/// while large files scale up automatically. See `storage::pick_chunk_size`.
fn default_chunk_size() -> u32 {
    8
}
/// Default multipart part concurrency. See [`UploadConfig::mpu_part_concurrency`]
/// and `storage::MPU_PART_CONCURRENCY` for the rationale behind 6.
fn default_mpu_concurrency() -> u32 {
    6
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeConfig {
    /// Master toggle — false disables transcoding entirely. Off by
    /// default for the first public release: most users don't need
    /// re-encoding, and a wrong codec/CRF choice can degrade footage
    /// silently. Power users who understand the tradeoffs can flip it
    /// on from the Transcode settings pane.
    #[serde(default)]
    pub enabled: bool,
    /// Target video codec (hevc, h264)
    #[serde(default = "default_codec")]
    pub codec: String,
    /// Constant Rate Factor (0–51, lower = better quality, larger file)
    #[serde(default = "default_crf")]
    pub crf: u8,
    /// x265 encoding preset (ultrafast..veryslow). Slower = better compression.
    #[serde(default = "default_preset")]
    pub preset: String,
    /// Target average bitrate in Mbps (VBR target). Typical 10.
    #[serde(default = "default_target_bitrate")]
    pub target_bitrate_mbps: u32,
    /// VBR peak ceiling in Mbps. When target < max, VideoToolbox allows
    /// bursts up to the cap; when they're set equal the encoder treats it as
    /// a tight ceiling and tends to undershoot the target.
    /// Typical 20 (i.e. 2× the target).
    #[serde(default = "default_max_bitrate")]
    pub max_bitrate_mbps: u32,
    /// Heights above this are downscaled (maintaining aspect ratio)
    #[serde(default = "default_max_height")]
    pub max_height: u32,
    /// Audio bitrate in kbps
    #[serde(default = "default_audio_bitrate")]
    pub audio_bitrate_kbps: u32,
    /// Target frame rate (0 = keep original)
    #[serde(default)]
    pub target_fps: u32,
    /// Hardware acceleration: "auto" | "videotoolbox" | "none".
    /// "auto" prefers a platform HW encoder when the installed ffmpeg has one,
    /// and falls back to the software encoder silently otherwise.
    #[serde(default = "default_hw_accel")]
    pub hw_accel: String,
}

fn default_codec() -> String {
    "hevc".to_string()
}
fn default_crf() -> u8 {
    23
}
fn default_preset() -> String {
    "medium".to_string()
}
fn default_target_bitrate() -> u32 {
    10
}
fn default_max_bitrate() -> u32 {
    20
}
fn default_max_height() -> u32 {
    1080
}
fn default_audio_bitrate() -> u32 {
    128
}
fn default_hw_accel() -> String {
    "auto".to_string()
}

impl Default for TranscodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            codec: default_codec(),
            crf: default_crf(),
            preset: default_preset(),
            target_bitrate_mbps: default_target_bitrate(),
            max_bitrate_mbps: default_max_bitrate(),
            max_height: default_max_height(),
            audio_bitrate_kbps: default_audio_bitrate(),
            target_fps: 0,
            hw_accel: default_hw_accel(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraConfig {
    #[serde(default = "default_true")]
    pub auto_detect: bool,
    #[serde(default)]
    pub auto_import: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    /// Tracing-subscriber EnvFilter directive — supports per-crate levels
    /// (e.g. `info,lw_app=trace`). Accepts a plain level too (e.g. `debug`).
    #[serde(default = "default_log_filter")]
    pub log_filter: String,
}

fn default_log_filter() -> String {
    crate::logging::DEFAULT_LOG_FILTER.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchFolderEntry {
    pub path: PathBuf,
    pub tenant_id: String,
    pub project_id: String,
    #[serde(default = "default_file_filter")]
    pub file_filter: Vec<String>,
}

fn default_file_filter() -> Vec<String> {
    vec!["video/*".to_string(), "application/pdf".to_string()]
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                environment: default_environment(),
                proxy_url: None,
            },
            upload: UploadConfig {
                auto_clean: false,
                bandwidth_limit_mbps: 0,
                max_concurrent_uploads: 2,
                chunk_size_mb: 8,
                sequential_uploads: false,
                mpu_part_concurrency: default_mpu_concurrency(),
            },
            transcode: TranscodeConfig::default(),
            camera: CameraConfig {
                auto_detect: true,
                auto_import: false,
            },
            app: GeneralConfig {
                auto_start: false,
                minimize_to_tray: true,
                log_filter: default_log_filter(),
            },
            watch_folders: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("linewise-desktop")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    pub fn data_dir() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("linewise-desktop")
    }

    pub fn db_path() -> PathBuf {
        Self::data_dir().join("linewise.db")
    }

    pub fn load() -> Result<Self, crate::error::ConfigError> {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&content)?)
        } else {
            let config = Self::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<(), crate::error::ConfigError> {
        let dir = Self::config_dir();
        std::fs::create_dir_all(&dir)?;
        let content = toml::to_string_pretty(self)?;
        std::fs::write(Self::config_path(), content)?;
        Ok(())
    }
}
