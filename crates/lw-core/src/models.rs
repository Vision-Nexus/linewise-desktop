use serde::{Deserialize, Serialize};

/// Mirrors backend Tenant type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tenant {
    pub id: String,
    pub name: String,
}

/// Mirrors backend Project type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// Mirrors backend ReferenceDocument type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceDocument {
    pub id: String,
    pub collection: String,
    pub description: String,
    pub metadata: DocumentMetadata,
    pub gcs_uri: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMetadata {
    pub filename: String,
    pub size: u64,
    pub mime_type: Option<String>,
}

/// Request to create a document
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentRequest {
    pub collection: String,
    pub description: String,
    pub metadata: CreateDocumentMetadata,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocumentMetadata {
    pub filename: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Signed upload URL response
#[derive(Debug, Deserialize)]
pub struct SignedUploadUrl {
    pub url: String,
    pub uri: String,
    pub expires: i64,
}

/// Firebase Auth tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub id_token: String,
    pub refresh_token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Current authenticated user info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    pub uid: String,
    pub email: String,
    pub display_name: Option<String>,
    pub tenants: Vec<Tenant>,
}

/// Upload task state persisted in SQLite
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadState {
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
}

/// Upload task record
#[derive(Debug, Clone)]
pub struct UploadTask {
    pub id: String,
    pub local_path: String,
    pub filename: String,
    pub size: u64,
    pub mime_type: String,
    pub tenant_id: String,
    pub project_id: String,
    pub document_id: Option<String>,
    pub gcs_session_uri: Option<String>,
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
