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
    #[error("Video is unplayable: {reason}")]
    VideoUnplayable { reason: String },
    #[error("Upload cancelled")]
    Cancelled,
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Database error: {0}")]
    Database(#[from] DbError),
}

impl UploadError {
    /// True for outcomes that are part of normal user flow (a duplicate, a
    /// cancelled upload, a video the user picked but is structurally
    /// unplayable). Real failures (network, IO, API, DB) return false.
    ///
    /// `sentry-tracing` captures `error!` events but only treats `warn!`
    /// as breadcrumbs, so routing expected outcomes through `log()` below
    /// keeps them out of Sentry alerts while leaving real failures loud.
    pub fn is_expected(&self) -> bool {
        match self {
            UploadError::Duplicate { .. }
            | UploadError::Cancelled
            | UploadError::VideoUnplayable { .. } => true,
            UploadError::FileNotFound(_)
            | UploadError::FileTooLarge { .. }
            | UploadError::Api { .. }
            | UploadError::GcsUpload { .. }
            | UploadError::Network(_)
            | UploadError::Io(_)
            | UploadError::Database(_) => false,
        }
    }

    /// Emits a tracing event at the right level for this error: `warn!`
    /// for expected outcomes, `error!` for real failures. `context` is a
    /// short prefix the call site supplies (e.g. `"Upload"`, `"Retry"`,
    /// or `format_args!("Upload of {}", filename)`) so the log line reads
    /// naturally without forcing the caller to allocate an intermediate
    /// `String`.
    pub fn log(&self, context: impl std::fmt::Display) {
        if self.is_expected() {
            tracing::warn!("{context} rejected: {self}");
        } else {
            tracing::error!("{context} failed: {self}");
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VideoValidationError {
    #[error("ffprobe not found in PATH")]
    FfprobeNotFound,
    #[error("Failed to probe video: {0}")]
    ProbeFailed(String),
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),
    /// Container is structurally unreadable — typically a missing `moov`
    /// atom from a power-cut MP4/MOV recording, or a truncated header.
    /// Distinct from `ProbeFailed` (internal/transient) and
    /// `UnsupportedFormat` (codec we don't handle): this file has no
    /// playable timeline and never will.
    #[error("Video is unplayable: {reason}")]
    Unplayable { reason: String },
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
