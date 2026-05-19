use serde::{Deserialize, Serialize};

/// Mirrors backend TenantInfo (from UserModels.scala). `parentGroupId`
/// is `Option[GroupId]` server-side: tenants admin-created without a
/// group come back with `None`. We currently surface the value through
/// `Tenant::is_in_group` so the sidebar can tag vision-lab tenants
/// with a badge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tenant {
    pub id: String,
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub parent_group_id: Option<String>,
}

impl Tenant {
    /// True when this tenant belongs to the given group. Returns false
    /// for tenants without a group as well as tenants in a different
    /// group; the caller doesn't need to disambiguate.
    pub fn is_in_group(&self, group_id: &str) -> bool {
        self.parent_group_id.as_deref() == Some(group_id)
    }
}

/// Mirrors backend Project type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// Mirrors backend DocumentResponse (DocumentModels.scala)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentResponse {
    pub id: String,
    pub project_id: String,
    pub collection: String,
    pub metadata: DocumentMeta,
    pub creator: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub deleted_at: Option<String>,
    pub gcs_uri: Option<String>,
    pub folder: Option<String>,
    // Skip complex nested types we don't need on the client
    #[serde(default)]
    pub rag: Option<serde_json::Value>,
    #[serde(default)]
    pub masking_config: Option<serde_json::Value>,
}

/// Mirrors backend DocumentMeta (DocumentModels.scala)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMeta {
    pub filename: String,
    pub mime_type: String,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub md5_hash: Option<String>,
    // Skip transcode/videoMeta/masking — not needed on desktop client
    #[serde(default)]
    pub transcode: Option<serde_json::Value>,
    #[serde(default)]
    pub video_meta: Option<serde_json::Value>,
    #[serde(default)]
    pub masking: Option<serde_json::Value>,
}

/// Mirrors backend CreateDocumentRequest (DocumentModels.scala)
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentRequest {
    pub collection: String,
    pub description: String,
    pub metadata: CreateDocumentMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Source-file MD5 (32 lowercase hex chars), pre-transcode. Lives at
    /// the top level — NOT inside `metadata`. The backend persists this
    /// to a dedicated `documents.original_md5` column with a regex CHECK
    /// and a `BEFORE UPDATE` immutability trigger; a `BEFORE INSERT`
    /// trigger on the registry mirrors it into `public.document_file_hashes`
    /// so future dedup-checks can match. Putting it inside `metadata`
    /// silently drops the value because `DocumentMeta` does not declare
    /// the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_md5: Option<String>,
}

/// Metadata for CreateDocumentRequest — matches DocumentMeta fields
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentMeta {
    pub filename: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

/// Request body for `POST /api/org/{tenant}/dedup-checks` — batch up
/// to 100 source-file MD5s. The backend rejects empty arrays and
/// arrays longer than 100 with a typed 400.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupCheckRequest<'a> {
    pub md5_hashes: &'a [String],
}

/// One match row inside the calling tenant for a queried hash. The
/// list is filtered by the caller's project access — matches in
/// projects the user cannot read are omitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupCheckMatch {
    pub document_id: String,
    pub project_id: String,
    pub document_created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupCheckResult {
    pub md5_hash: String,
    /// Documents in the calling tenant carrying this hash, restricted
    /// to projects the caller can read.
    pub tenant_matches: Vec<DedupCheckMatch>,
    /// Distinct documents the same calling user uploaded with this
    /// hash in other tenants they belong to. A count, not a list —
    /// other tenants' project structure is intentionally not exposed.
    pub user_other_tenant_count: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupCheckResponse {
    pub results: Vec<DedupCheckResult>,
}

/// Mirrors backend PresignedUrlResponse (GCSModels.scala)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresignedUrlResponse {
    pub url: String,
    pub uri: String,
    pub expires: String,
    #[serde(default)]
    pub fields: Option<serde_json::Value>,
}

/// Firebase Auth tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub id_token: String,
    pub refresh_token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// WhoAmI response from backend (UserRouteHelpers.scala)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhoAmIResponse {
    pub firebase: FirebaseUserInfo,
    pub user: Option<WhoAmIUser>,
}

/// Firebase user from WhoAmI (subset of FirebaseUser fields we need)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirebaseUserInfo {
    pub uid: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub picture: Option<String>,
    #[serde(default)]
    pub is_email_verified: bool,
}

/// User from WhoAmI response (from UserModels.scala)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhoAmIUser {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub tenants: Vec<String>,
    pub tenant_infos: Option<Vec<Tenant>>,
}

/// Convenience type used in app state. `system_roles` is decoded
/// locally from the Firebase ID token (see `auth::claims`); the
/// backend whoami response doesn't expose it directly. Empty for
/// ordinary tenant users.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub uid: String,
    pub email: String,
    pub display_name: Option<String>,
    pub photo_url: Option<String>,
    pub tenants: Vec<Tenant>,
    pub system_roles: Vec<String>,
}

impl UserInfo {
    pub fn from_whoami(resp: WhoAmIResponse, system_roles: Vec<String>) -> Option<Self> {
        let user = resp.user?;
        let tenants = user.tenant_infos.unwrap_or_default();
        Some(Self {
            uid: resp.firebase.uid,
            email: user.email,
            display_name: resp.firebase.name,
            photo_url: resp.firebase.picture,
            tenants,
            system_roles,
        })
    }

    pub fn is_system_user(&self) -> bool {
        !self.system_roles.is_empty()
    }

    /// True only for the `admin` system role. The other system roles
    /// (`viewer`, `probe`) can read system data but should not see
    /// destructive affordances like the dedup bypass that lets a
    /// rejected upload proceed anyway.
    pub fn is_super_admin(&self) -> bool {
        self.system_roles.iter().any(|r| r == "admin")
    }
}

/// Upload task state persisted in SQLite
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadState {
    /// Row was just inserted from a file pick / drop and the server
    /// quality check is in flight. The atom walk + the
    /// `/quality-check` round-trip happen here; the row carries no
    /// `video_info` or warnings yet because both come from the
    /// response. The UI shows an indeterminate ("loading") progress
    /// bar so the user can see that work is happening but no estimate
    /// is available. The row transitions to `Hashing` on accept (or
    /// server-reject — we still hash for super-admin force-upload)
    /// and to `Rejected` directly when the local atom walk says the
    /// file is broken (no `moov`, unsupported container) or the
    /// server is unreachable. Holding this as its own state keeps
    /// broken-file rows visible with a typed reason rather than
    /// hidden behind a transient toast.
    QualityChecking,
    /// Row was just inserted from a file pick / drop. The video probe
    /// has run (synchronously, fast) so `video_info` and quality
    /// reasons are present, but BLAKE3+MD5 hashing and the
    /// cross-tenant dedup check are still in flight on a background
    /// task. The UI shows a progress bar driven by `HashProgress`
    /// events; the row transitions to `Staged` or `Rejected` when the
    /// background work finishes. We carry this as its own state — not
    /// a sub-state of `Staged` — because a hashing row must not
    /// appear in the "Ready to Upload" count: confirming staged tasks
    /// before their hash lands would skip the dedup gate entirely.
    Hashing,
    Staged,
    /// Source video failed the acceptance-quality gate (bitrate, fps,
    /// resolution, or device-fingerprint below the configured floor). The
    /// row stays visible so the user can see *why* the file is unusable,
    /// but it never advances to PENDING — the confirm-staged path skips it.
    /// Distinct from `Failed`: a rejected file was refused before any
    /// upload work happened, on grounds that won't change without picking
    /// a different file.
    Rejected,
    Pending,
    Validating,
    Transcoding,
    Desensitizing,
    Creating,
    Uploading,
    Verifying,
    Completed,
    Failed,
    Paused,
}

impl UploadState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::QualityChecking => "QUALITY_CHECKING",
            Self::Hashing => "HASHING",
            Self::Staged => "STAGED",
            Self::Rejected => "REJECTED",
            Self::Pending => "PENDING",
            Self::Validating => "VALIDATING",
            Self::Transcoding => "TRANSCODING",
            Self::Desensitizing => "DESENSITIZING",
            Self::Creating => "CREATING",
            Self::Uploading => "UPLOADING",
            Self::Verifying => "VERIFYING",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Paused => "PAUSED",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "QUALITY_CHECKING" => Self::QualityChecking,
            "HASHING" => Self::Hashing,
            "STAGED" => Self::Staged,
            "REJECTED" => Self::Rejected,
            "PENDING" => Self::Pending,
            "VALIDATING" => Self::Validating,
            "TRANSCODING" => Self::Transcoding,
            "DESENSITIZING" => Self::Desensitizing,
            "CREATING" => Self::Creating,
            "UPLOADING" => Self::Uploading,
            "VERIFYING" => Self::Verifying,
            "COMPLETED" => Self::Completed,
            "FAILED" => Self::Failed,
            "PAUSED" => Self::Paused,
            _ => Self::Pending,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Pending
                | Self::Validating
                | Self::Transcoding
                | Self::Desensitizing
                | Self::Creating
                | Self::Uploading
                | Self::Verifying
        )
    }
}

/// Upload task record
#[derive(Debug, Clone, PartialEq)]
pub struct UploadTask {
    pub id: String,
    pub local_path: String,
    pub filename: String,
    pub size: u64,
    pub mime_type: String,
    pub tenant_id: String,
    pub project_id: String,
    pub document_id: Option<String>,
    pub session_id: Option<String>,
    pub bytes_uploaded: u64,
    pub state: UploadState,
    pub error_message: Option<String>,
    pub hash: Option<String>,
    /// MD5 of the source file (pre-transcode), 32 lowercase hex
    /// chars. Computed alongside [`hash`] in a single I/O pass at
    /// staging time. Sent to the backend in two places: `originalMd5`
    /// on document creation, and the `dedup-checks` request body.
    /// `None` on rows staged before this field existed.
    pub source_md5: Option<String>,
    /// Advisory lines (warn-coloured in the UI). Recommend-band hints,
    /// telemetry advisories, missing-device-fingerprint nudges. Not
    /// blocking — a row can be `Staged` and still carry warnings.
    pub validation_warnings: Vec<String>,
    /// Hard reject reasons (error-coloured in the UI). Acceptance-band
    /// failures, blocking provenance issues. Populated only when the
    /// row's terminal state is `Rejected`. The split from
    /// `validation_warnings` lets the UI render severity through
    /// colour without parsing message strings.
    pub rejection_reasons: Vec<String>,
    pub retry_count: u32,
    /// User opted in to transcode this file before upload
    pub transcode: bool,
    /// Size of the transcoded artifact in bytes. `None` until transcode
    /// completes (and remains `None` if transcode was disabled for this task).
    pub transcoded_size: Option<u64>,
    /// Video probe info (populated at staging time for video files)
    pub video_info: Option<VideoInfo>,
    /// Super-admin override: when true, the upload engine skips both
    /// the cross-tenant dedup gate and the local-DB duplicate
    /// short-circuit on this task. Persisted so a re-staged row keeps
    /// its bypass across an app restart. Set only by the "Force upload"
    /// affordance in the UI, which is itself gated on
    /// [`UserInfo::is_super_admin`].
    pub force_upload: bool,
}

/// Video probe result
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub bitrate_kbps: u64,
    pub codec: String,
    pub audio_codec: String,
    pub duration_secs: f64,
    pub format: String,
    /// Every readable container + per-stream tag from the source file,
    /// in container-then-stream order. Per-stream keys are namespaced
    /// like `video/handler_name`, `data/creation_time` so they don't
    /// collide with the container-scope variant.
    #[serde(default)]
    pub metadata: Vec<(String, String)>,
    /// Detected telemetry stream label (e.g. "DJI CAM metadata", "GoPro
    /// telemetry (GPMF)") when the file carries one. We don't decode the
    /// binary payload — just surface the fact that it's there.
    #[serde(default)]
    pub telemetry: Option<String>,
}

/// Acceptance verdict from the server-side quality check.
/// `Accepted` lets the file proceed to STAGED; `Rejected` carries the
/// user-facing reasons and routes the task into the REJECTED state.
///
/// Wire shape (mirrors the Scala `VideoQualityResult.acceptance`
/// case-class hierarchy with the default external-tag serde encoding):
///   * `"Accepted"` → [`Self::Accepted`]
///   * `{"Rejected": {"reasons": [...]}}` → [`Self::Rejected`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Acceptance {
    Accepted,
    Rejected { reasons: Vec<String> },
}

/// Response body of `POST /api/org/{tenant}/projects/{pid}/quality-check`.
/// Mirrors the Scala `VideoQualityResult` case class.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityCheckResponse {
    pub acceptance: Acceptance,
    /// Advisory lines (warn-coloured in the UI) — recommend-band hints,
    /// telemetry advisories, missing-device-fingerprint nudges. Not
    /// blocking — a row can be `Accepted` and still carry warnings.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Probed parameters of the source clip (resolution, fps, codec,
    /// duration, container metadata, telemetry label). Used by the
    /// client to render the per-row info popover.
    pub info: VideoInfo,
}
