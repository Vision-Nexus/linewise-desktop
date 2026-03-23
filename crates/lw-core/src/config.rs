use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub upload: UploadConfig,
    pub desensitization: DesensitizationConfig,
    pub camera: CameraConfig,
    pub app: GeneralConfig,
    #[serde(default)]
    pub watch_folders: Vec<WatchFolderEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_environment")]
    pub environment: Environment,
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
            Self::Production => "https://api.app.linewise.io",
        }
    }
}

fn default_environment() -> Environment {
    Environment::Production
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadConfig {
    #[serde(default = "default_true")]
    pub auto_clean: bool,
    #[serde(default)]
    pub bandwidth_limit_mbps: u32,
    #[serde(default = "default_concurrent")]
    pub max_concurrent_uploads: u32,
    #[serde(default = "default_chunk_size")]
    pub chunk_size_mb: u32,
}

fn default_true() -> bool {
    true
}
fn default_concurrent() -> u32 {
    3
}
fn default_chunk_size() -> u32 {
    8
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesensitizationConfig {
    #[serde(default = "default_true")]
    pub strip_metadata: bool,
    #[serde(default)]
    pub blur_faces: bool,
    #[serde(default)]
    pub blur_license_plates: bool,
    #[serde(default = "default_processing_mode")]
    pub processing_mode: ProcessingMode,
    #[serde(default)]
    pub remote_api_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessingMode {
    Local,
    Remote,
}

fn default_processing_mode() -> ProcessingMode {
    ProcessingMode::Local
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
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_level() -> String {
    "info".to_string()
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
                environment: Environment::Production,
            },
            upload: UploadConfig {
                auto_clean: true,
                bandwidth_limit_mbps: 0,
                max_concurrent_uploads: 3,
                chunk_size_mb: 8,
            },
            desensitization: DesensitizationConfig {
                strip_metadata: true,
                blur_faces: false,
                blur_license_plates: false,
                processing_mode: ProcessingMode::Local,
                remote_api_url: String::new(),
            },
            camera: CameraConfig {
                auto_detect: true,
                auto_import: false,
            },
            app: GeneralConfig {
                auto_start: false,
                minimize_to_tray: true,
                log_level: "info".to_string(),
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
