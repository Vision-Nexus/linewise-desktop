use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Token expired")]
    TokenExpired,
    #[error("Email not verified")]
    EmailNotVerified,
    #[error("Account disabled")]
    AccountDisabled,
    #[error("MFA required")]
    MfaRequired { pending_token: String },
    #[error("No stored credentials")]
    NoStoredCredentials,
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Firebase error ({code}): {message}")]
    Firebase { code: String, message: String },
    #[error("Keyring error: {0}")]
    Keyring(String),
    #[error("OAuth error ({provider}): {message}")]
    OAuth { provider: String, message: String },
    #[error("Sign-in cancelled")]
    UserCancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),
    #[error("File too large: {size} bytes (max {max} bytes)")]
    FileTooLarge { size: u64, max: u64 },
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("GCS upload failed at byte {offset}")]
    GcsUpload {
        offset: u64,
        #[source]
        source: reqwest::Error,
    },
    #[error("Duplicate detected: document {existing_id}")]
    Duplicate { existing_id: String },
    #[error("Upload cancelled")]
    Cancelled,
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Database error: {0}")]
    Database(#[from] DbError),
}

#[derive(Debug, thiserror::Error)]
pub enum VideoValidationError {
    #[error("ffprobe not found in PATH")]
    FfprobeNotFound,
    #[error("Failed to probe video: {0}")]
    ProbeFailed(String),
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),
}

#[derive(Debug, thiserror::Error)]
pub enum TranscodeError {
    #[error("FFmpeg library not available")]
    FfmpegNotAvailable,
    #[error("Codec not found: {0} (is libx265 compiled into FFmpeg?)")]
    CodecNotFound(String),
    #[error("Encoding failed: {0}")]
    EncodingFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("Migration error: {0}")]
    Migration(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("Serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Auth error: {0}")]
    Auth(#[from] AuthError),
    #[error("Upload error: {0}")]
    Upload(#[from] UploadError),
    #[error("Video error: {0}")]
    Video(#[from] VideoValidationError),
    #[error("Transcode error: {0}")]
    Transcode(#[from] TranscodeError),
    #[error("Database error: {0}")]
    Database(#[from] DbError),
    #[error("Config error: {0}")]
    Config(#[from] ConfigError),
}
