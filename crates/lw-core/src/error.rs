use crate::container_kind::ContainerKind;
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
    /// Cross-tenant dedup check rejected this file. Either the calling
    /// tenant already holds a document with the same source-file MD5
    /// (in a project the user can read), or the same user uploaded
    /// this file in another tenant they belong to. Distinct from
    /// [`Self::Duplicate`], which is the local SQLite hash cache hit.
    #[error(
        "Already uploaded: {tenant_match_count} in this tenant, {user_other_tenant_count} in other tenants you belong to"
    )]
    DuplicateOnServer {
        tenant_match_count: usize,
        user_other_tenant_count: u64,
    },
    #[error("Video is unplayable: {reason}")]
    VideoUnplayable { reason: String },
    /// The picked file's magic bytes don't match an ISO BMFF (mp4 / mov)
    /// container. The 2026-05-16 production-data sweep showed 99.98% of
    /// real customer uploads are ISO BMFF, so we reject the rest with a
    /// kind-specific message at staging time — before the atom walker
    /// runs and before the network round-trip — instead of letting them
    /// hit the server and return a less helpful error.
    #[error(
        "Linewise supports mp4 and mov files; this file appears to be {kind_label}. Please export from your camera or NLE as mp4 or mov.",
        kind_label = kind.human_label()
    )]
    UnsupportedContainer { kind: ContainerKind },
    /// The reconstructed metadata payload exceeds the 8 MiB hard cap.
    /// Real-world camera output sits well below 1 MiB, so this almost
    /// always means the input was a fragmented or pathologically-shaped
    /// container the desktop can't summarise without sending media bytes.
    /// Surfaced separately from `Api { 413 }` so the UI can render a
    /// dedicated message rather than a generic API error.
    #[error("Video metadata too large: {bytes} bytes exceeds {cap} byte cap")]
    QualityCheckPayloadTooLarge { bytes: u64, cap: u64 },
    /// Server unreachable for the quality-check round-trip. After the
    /// hard cutover the desktop has no local rule evaluator, so a
    /// network-down launch is now a user-visible step backwards. The
    /// UI renders "Server unreachable — quality check requires a network
    /// connection" instead of a generic API/network error.
    #[error("Quality check unavailable — server unreachable")]
    QualityCheckOffline {
        #[source]
        source: reqwest::Error,
    },
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
            | UploadError::DuplicateOnServer { .. }
            | UploadError::Cancelled
            | UploadError::VideoUnplayable { .. }
            | UploadError::UnsupportedContainer { .. }
            | UploadError::QualityCheckPayloadTooLarge { .. }
            | UploadError::QualityCheckOffline { .. } => true,
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
    /// The reconstructed metadata payload would exceed the 8 MiB hard
    /// cap. Real-world camera output stays well below 1 MiB, so this
    /// almost always means a malformed input. We refuse to ship the
    /// payload in this case.
    #[error("Video metadata too large: {bytes} bytes exceeds {cap} byte cap")]
    MoovTooLarge { bytes: u64, cap: u64 },
    /// Failed to read the input file while walking atoms.
    #[error("Failed to read video header: {0}")]
    Io(#[from] std::io::Error),
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
pub enum VersionCheckError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Decode error: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("Bad version string {input:?}: {source}")]
    BadVersion {
        input: String,
        #[source]
        source: semver::Error,
    },
}

/// Failure modes for the title-bar Repair flow. Surfaces enough context
/// (which path, which slice) for the modal to render an actionable error
/// row instead of a generic "wipe failed" message.
#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error("Failed to remove {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Database reset failed: {0}")]
    Db(#[from] DbError),
    /// The repair task itself panicked or was cancelled by the runtime.
    /// We can't tell which slices succeeded and which didn't — the
    /// reported state for any selected slice is "unknown", not a
    /// fabricated I/O failure.
    #[error("Repair task did not complete: {reason} (state of selected slices unknown)")]
    TaskPanicked { reason: String },
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
