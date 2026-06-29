use crate::container_kind::ContainerKind;
use std::path::{Path, PathBuf};

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
    #[error(
        "Network unreachable after {attempts} attempts while refreshing your session. \
         Please check your internet connection, then close and reopen the app to retry."
    )]
    NetworkUnreachable { attempts: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),
    #[error("File too large: {size} bytes (max {max} bytes)")]
    FileTooLarge { size: u64, max: u64 },
    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },
    /// Authentication / session failure from `AuthService` (e.g. the Firebase
    /// token refresh couldn't reach `securetoken.googleapis.com` — a transport
    /// failure — or credentials were rejected). Carries the underlying message
    /// verbatim so the UI shows the real cause and any user guidance, instead of
    /// a misleading `API error (401)`.
    #[error("{message}")]
    Auth { message: String },
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
        "Already uploaded: {tenant_match_count} in this tenant, in {n_other_tenants} other tenant(s) you belong to",
        n_other_tenants = user_other_tenant_ids.len()
    )]
    DuplicateOnServer {
        tenant_match_count: usize,
        user_other_tenant_ids: Vec<String>,
    },
    #[error("Video is unplayable: {reason}")]
    VideoUnplayable { reason: String },
    /// Embedding the user-entered capture metadata into the file (via ffmpeg)
    /// failed before upload. Carries the ffmpeg error verbatim.
    #[error("Failed to embed capture metadata: {message}")]
    CaptureEmbed { message: String },
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
    /// The reconstructed metadata payload exceeds the 16 MiB hard cap.
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
    /// The local file changed size between the moment its size was captured
    /// (staging / resumable-session declaration) and the moment its bytes were
    /// read for upload: a recording still being written, a cloud-sync
    /// placeholder not yet materialized, or an antivirus/copy still in progress.
    /// The size declared to GCS no longer matches the bytes on disk, so a
    /// chunk/part read came up short or the resumable total can't be satisfied.
    /// Retrying a moving target never converges, so this is `is_expected` — the
    /// row settles with an actionable message and is NOT auto-retried (the
    /// message matches none of `get_failed_retryable`'s patterns); the user
    /// re-adds the file once it has finished writing/downloading.
    #[error(
        "File changed on disk during upload (expected {declared} bytes, file is now {actual}). \
         Wait until it finishes writing or downloading, then add it again."
    )]
    FileChangedDuringUpload { declared: u64, actual: u64 },
    /// The source file/path was gone when its bytes were read for upload —
    /// moved, renamed, deleted, or on a removable/network drive that
    /// disconnected mid-batch (Windows `os error 2` = file not found,
    /// `os error 3` = path not found, both mapped by std to
    /// `ErrorKind::NotFound`; ENOENT elsewhere).
    ///
    /// While the file is missing this is `is_expected` (a `warn!`, not a
    /// Sentry error) and is never AUTO-retried: its message matches none of
    /// the transient-transport markers in the `error_message LIKE …` allow-list
    /// of [`crate::db::Database::get_failed_retryable`], so the 30s auto-retry
    /// loop never re-queues it. The attempt fails fast at `open()`, so the
    /// upload slot is released immediately and the row settles in `Failed`
    /// without blocking other uploads. The created document, any resumable
    /// session, and `bytes_uploaded` are PRESERVED: once the user restores the
    /// file they click Retry to RESUME from the partial upload, or Remove to
    /// discard it.
    ///
    /// `path` is carried for logging only and is deliberately kept OUT of the
    /// `Display` string so that a path containing a marker word (e.g. a
    /// `\\NetworkShare\…` UNC path) can't leak into the message and be misread
    /// as a retryable network error by that `LIKE`-based gate.
    #[error(
        "Source file is missing — it was moved, renamed, or deleted during upload. \
         Restore it to its original location and click Retry to resume, or Remove to discard this upload."
    )]
    SourceFileMissing { path: PathBuf },
    /// A part PUT in the parallel multipart (XML MPU) path completed but
    /// returned no usable `ETag` response header. GCS always sets `ETag` on
    /// a successful part PUT; its absence means a misbehaving proxy stripped
    /// it or the URL pointed somewhere unexpected. Without the ETag the
    /// backend cannot complete the MPU, so we fail the part rather than send
    /// an empty tag. Distinct from [`Self::Api`] (which is a non-2xx) — this
    /// is a 2xx with a missing header.
    #[error("Multipart part {part_number}: upload succeeded but no ETag header was returned")]
    MpuMissingEtag { part_number: i32 },
    /// A multipart part task could not be joined (the spawned tokio task
    /// panicked or was aborted). Carries the part number and the join-error
    /// rendering so the failure is attributable rather than a generic
    /// "upload failed". Not a transport error — surfaced separately so it is
    /// never mistaken for a retryable network blip.
    #[error("Multipart part {part_number}: upload task did not complete: {reason}")]
    MpuTaskFailed { part_number: i32, reason: String },
    /// A multipart RESUME (app restart with a persisted `uploadId`) returned a
    /// plan that could not be reconciled with the local file — e.g. the
    /// server-reported part layout doesn't cover the expected part count, or a
    /// listed part falls outside the valid range. We refuse to complete with a
    /// mismatched part set (which GCS would reject as `InvalidPart` anyway) and
    /// surface this so the engine can abandon the stale upload and restart
    /// fresh. Not a transport error.
    #[error("Multipart resume {upload_id}: cannot reconcile resumed plan: {reason}")]
    MpuResumeFailed { upload_id: String, reason: String },
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
            | UploadError::QualityCheckOffline { .. }
            | UploadError::FileChangedDuringUpload { .. }
            // A missing source file/path is a user-environment condition (the
            // file was moved/deleted, or a removable drive vanished), not a
            // code fault — surface it as a warning, like FileChangedDuringUpload.
            | UploadError::SourceFileMissing { .. }
            | UploadError::FileNotFound(_) => true,
            // ENOENT (a stray missing-file IO that escaped `from_source_io`) or
            // ENOSPC / a full disk are user-environment conditions, not code
            // faults — keep them out of Sentry. Other IO / DB errors stay loud.
            UploadError::Io(e) => io_is_missing_or_full(e),
            UploadError::Database(e) => e.to_string().contains("disk is full"),
            UploadError::FileTooLarge { .. }
            | UploadError::Api { .. }
            | UploadError::Auth { .. }
            | UploadError::GcsUpload { .. }
            | UploadError::MpuMissingEtag { .. }
            | UploadError::MpuTaskFailed { .. }
            | UploadError::MpuResumeFailed { .. }
            | UploadError::Network(_)
            | UploadError::CaptureEmbed { .. } => false,
        }
    }

    /// Map an IO error from opening/statting the upload source against `path`
    /// into the most specific variant: a missing file/path (ENOENT; Windows
    /// `os error 2`/`3`) becomes the permanent, user-actionable
    /// [`Self::SourceFileMissing`]; everything else stays [`Self::Io`].
    pub fn from_source_io(err: std::io::Error, path: &Path) -> Self {
        if err.kind() == std::io::ErrorKind::NotFound {
            UploadError::SourceFileMissing {
                path: path.to_path_buf(),
            }
        } else {
            UploadError::Io(err)
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

/// True for a raw OS IO error that means the source file/path is gone
/// (ENOENT; Windows `os error 2`/`3` → `ErrorKind::NotFound`) or the disk is
/// full (ENOSPC = 28; Windows `ERROR_HANDLE_DISK_FULL` = 39,
/// `ERROR_DISK_FULL` = 112). Both are user-environment conditions, never a
/// transient transport hiccup, so they are neither Sentry-loud nor retryable.
fn io_is_missing_or_full(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
        || matches!(e.raw_os_error(), Some(28) | Some(39) | Some(112))
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
    /// The reconstructed metadata payload would exceed the 16 MiB hard
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
