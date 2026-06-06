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

/// Multi-signal pre-upload digest of the source file (pre-transcode).
/// Mirrors `linewise-api`'s `Digest` case class
/// (`features/document/Digest.scala`).
///
/// **JSON keys are emitted verbatim** by the backend's
/// `Codec.AsObject.derived[Digest]`. The underscores in
/// `sha256_head_256kib` are load-bearing — DO NOT add
/// `serde(rename_all = "camelCase")` to this struct, even if a future
/// cleanup unifies camelCase across DTOs. A camelCase rename here
/// silently makes the field unrecognised on the wire (the backend reads
/// the literal snake_case key) and the upload-time digest leg is lost
/// without any error — the GCS-callback `verified_digest` would still
/// land but the pre-upload dedup signal would degrade.
///
/// Each leg is `Option<String>` independently — desktop sends all three;
/// older / partial-data rows may carry only md5. The wire shapes are
/// constrained server-side:
///   - `md5` — 32 lowercase hex chars (`Md5Hash`).
///   - `crc32c` — base64 of 4 big-endian bytes, exactly 8 chars
///     (`[A-Za-z0-9+/]{6}==`), shape-compatible with GCS's
///     `x-goog-hash: crc32c=` (`Crc32c`).
///   - `sha256_head_256kib` — 64 lowercase hex chars over the first
///     262144 bytes of the source file (`Sha256Hex`).
#[derive(Debug, Serialize)]
pub struct Digest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crc32c: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256_head_256kib: Option<String>,
}

/// Mirrors backend CreateDocumentRequest (DocumentModels.scala — folded
/// V2 back into V1 in linewise-api PR #203). The pre-upload `digest`
/// field lives at the top level (NOT inside `metadata`) on purpose:
/// content hashes are permanent physical facts about the source bytes,
/// not lifecycle metadata. Placing them inside `metadata` would silently
/// drop them because `DocumentMeta` does not declare the keys.
///
/// The legacy single-md5 `originalMd5` field is intentionally absent —
/// the desktop is the canonical "digest provider" client, the backend
/// only falls back to `originalMd5` for older clients that haven't
/// migrated. Note: backend's `DocumentResponse` also exposes a separate
/// `originalDigest` field; we don't decode that here because the upload
/// finalisation path only needs `gcs_uri`.
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
    /// Pre-upload multi-signal digest. Desktop sends all three legs.
    /// Note: this `Option` is for the rare partial-staging case; in the
    /// happy path the desktop always sends `Some(_)` because
    /// `consume_hash_stream` populates all four hashes in one I/O pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<Digest>,
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

/// Request body for `POST /api/org/{tenant}/digest-checks` — the
/// multi-signal (V2) cross-tenant dedup query. Each candidate carries
/// any subset of `{md5, crc32c, sha256_head_256kib}` (desktop sends all
/// three legs of one file's digest). Unlike the legacy md5-only
/// `/dedup-checks`, the server can match on the verified
/// `(crc32c, sha256_head_256kib)` pair, so a file uploaded via a
/// resumable path — where GCS surfaces crc32c but never an md5 — is
/// still detected as a duplicate.
///
/// `sources` (remote gs:///Drive URLs) is intentionally omitted: the
/// desktop only ever dedups local files it has already hashed. The
/// backend treats a missing `sources` as `None`.
#[derive(Debug, Serialize)]
pub struct DigestCheckRequest<'a> {
    pub candidates: &'a [Digest],
}

/// One match row inside the calling tenant for a queried hash. The
/// list is filtered by the caller's project access — matches in
/// projects the user cannot read are omitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupCheckMatch {
    pub document_id: String,
    pub project_id: String,
    /// Tenant the document lives in. For same-tenant matches this
    /// equals the calling tenant (redundant with the URL but kept
    /// for symmetry with cross-tenant payloads). The desktop joins
    /// this against its locally-cached `whoami` tenant list to
    /// render a friendly display name.
    pub tenant_id: String,
    /// Linewise UserId (UUID) of the original uploader. Lets the
    /// client tell "you uploaded this" apart from "a teammate
    /// uploaded this" without a follow-up document GET.
    pub creator_id: String,
    pub document_created_at: String,
}

/// One row of V2 dedup output for a queried candidate. Mirrors backend
/// `DigestCheckResult` (DigestCheckModels.scala). The echoed `candidate`
/// field and each match's `matchType` tag are present on the wire but
/// intentionally not deserialized here: the desktop sends exactly one
/// candidate per request (so `results` carries at most one row and no
/// correlation is needed), and the Allow/Reject/Reuse verdict keys off
/// the match *lists*, not the confidence tag.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestCheckResult {
    /// Documents in the calling tenant carrying this digest, restricted
    /// to projects the caller can read.
    pub tenant_matches: Vec<DedupCheckMatch>,
    /// Distinct tenants OTHER than the calling tenant in which the
    /// same calling user uploaded this digest. IDs only — other
    /// tenants' project structure is intentionally not exposed.
    /// The desktop maps each id to its locally-known tenant
    /// `display_name` from `whoami`.
    pub user_other_tenant_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestCheckResponse {
    pub results: Vec<DigestCheckResult>,
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
    /// chars. Computed alongside [`hash`] / [`source_crc32c`] /
    /// [`source_sha256_head_256kib`] in a single I/O pass at staging
    /// time. Sent to the backend in two places: as `digest.md5` on
    /// document creation (multi-signal Digest), and as the sole key
    /// of the `dedup-checks` request body (the cross-tenant dedup
    /// registry remains md5-keyed). `None` on rows staged before this
    /// field existed.
    pub source_md5: Option<String>,
    /// CRC32C of the source file, base64-encoded with `==` padding (8
    /// chars total). Big-endian byte order to match GCS's
    /// `x-goog-hash: crc32c=` shape — desktop-supplied value is then
    /// directly comparable to the post-upload
    /// `verified_digest.crc32c`. Sent as `digest.crc32c`. `None` on
    /// rows staged before the multi-signal-digest migration.
    pub source_crc32c: Option<String>,
    /// SHA-256 over the first 262144 bytes of the source file (or
    /// whole file if shorter), 64 lowercase hex chars. Sent as
    /// `digest.sha256_head_256kib`. `None` on rows staged before the
    /// multi-signal-digest migration.
    pub source_sha256_head_256kib: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the wire shape of `CreateDocumentRequest` against the backend's
    /// `Codec.AsObject.derived[Digest]`. Two regressions this catches:
    ///   1. A future `serde(rename_all = "camelCase")` on `Digest` would
    ///      emit `sha256Head_256kib` (or similar) — the backend would
    ///      ignore the field silently. The literal snake_case key is
    ///      load-bearing.
    ///   2. A revival of the legacy `originalMd5` field would route via
    ///      `Digest.fromLegacy` server-side, defeating the whole reason
    ///      for sending the multi-signal `digest`.
    #[test]
    fn create_document_request_wire_shape() {
        let body = CreateDocumentRequest {
            collection: "documents".to_string(),
            description: "x".to_string(),
            metadata: CreateDocumentMeta {
                filename: "x.mp4".to_string(),
                mime_type: "video/mp4".to_string(),
                size: Some(1),
            },
            model_name: None,
            folder: None,
            digest: Some(Digest {
                md5: Some("d41d8cd98f00b204e9800998ecf8427e".to_string()),
                crc32c: Some("AAAAAA==".to_string()),
                sha256_head_256kib: Some(
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
                ),
            }),
        };
        let v = serde_json::to_value(&body).expect("serialise");
        assert!(
            v.pointer("/digest/sha256_head_256kib").is_some(),
            "snake_case key must be emitted verbatim, got: {v}"
        );
        assert!(v.pointer("/digest/md5").is_some());
        assert!(v.pointer("/digest/crc32c").is_some());
        assert!(
            v.get("originalMd5").is_none(),
            "legacy originalMd5 field must NOT appear on the wire"
        );
    }
}
