use serde::{Deserialize, Serialize};

/// Mirrors backend TenantInfo (from UserModels.scala)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tenant {
    pub id: String,
    pub name: String,
    pub display_name: String,
}

/// Mirrors backend Project type
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default)]
    pub is_email_verified: bool,
}

/// User from WhoAmI response (from UserModels.scala)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhoAmIUser {
    pub id: String,
    pub email: String,
    pub tenant: String,
    #[serde(default)]
    pub tenants: Vec<String>,
    pub tenant_infos: Option<Vec<Tenant>>,
}

/// Convenience type used in app state
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub uid: String,
    pub email: String,
    pub display_name: Option<String>,
    pub tenants: Vec<Tenant>,
}

impl UserInfo {
    pub fn from_whoami(resp: WhoAmIResponse) -> Option<Self> {
        let user = resp.user?;
        let tenants = user.tenant_infos.unwrap_or_default();
        Some(Self {
            uid: resp.firebase.uid,
            email: user.email,
            display_name: resp.firebase.name,
            tenants,
        })
    }
}

/// Upload task state persisted in SQLite
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadState {
    Staged,
    Pending,
    Validating,
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
            Self::Staged => "STAGED",
            Self::Pending => "PENDING",
            Self::Validating => "VALIDATING",
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
            "STAGED" => Self::Staged,
            "PENDING" => Self::Pending,
            "VALIDATING" => Self::Validating,
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
    pub validation_warnings: Vec<String>,
    pub retry_count: u32,
}

/// Video probe result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub bitrate_kbps: u64,
    pub codec: String,
    pub duration_secs: f64,
    pub format: String,
}

#[derive(Debug, Clone)]
pub struct VideoValidationResult {
    pub info: VideoInfo,
    pub warnings: Vec<String>,
}
