use dioxus::prelude::*;
use lw_core::api_client::ApiClient;
use lw_core::auth::{AuthClientConfig, AuthService};
use lw_core::config::AppConfig;
use lw_core::db::Database;
use lw_core::models::{Project, Tenant, UploadTask, UserInfo};
use lw_core::upload::{UploadEngine, UploadEvent};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

const FIREBASE_API_KEY: &str = "AIzaSyDqUP3c44v-S22hyPJdjSTCNAFai_-3914";
// OAuth client IDs for Google and Microsoft. These are "installed app" /
// "public client" IDs — not secrets; PKCE proves possession. Register one
// OAuth client of type "Desktop app" per provider in the `linewise-455019`
// GCP project and in the Azure AD app registration, then paste the IDs here.
// Keeping them next to FIREBASE_API_KEY mirrors current practice; if/when
// env-specific config is needed, both blocks move into AppConfig together.
const GOOGLE_OAUTH_CLIENT_ID: &str =
    "3295823160-3im6h5df26g00nh8cnb623kn3i59onkc.apps.googleusercontent.com";
// Google's token endpoint requires the Desktop-app client_secret even with
// PKCE. Per Google's own docs this value is not confidential — it ships in
// the binary — so treat it as public identifying material, not as a secret.
const GOOGLE_OAUTH_CLIENT_SECRET: &str = "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";
const MICROSOFT_OAUTH_CLIENT_ID: &str = "e83e590c-33fd-4361-8063-b93e95206a14";

#[derive(Clone)]
#[allow(dead_code)]
pub struct CoreServices {
    pub auth: Arc<AuthService>,
    pub api: Arc<ApiClient>,
    pub db: Arc<Database>,
    pub upload_engine: Arc<UploadEngine>,
    pub config: AppConfig,
    pub event_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<UploadEvent>>>,
}

impl CoreServices {
    pub async fn init() -> Result<Self, String> {
        let config = AppConfig::load().map_err(|e| format!("Config error: {e}"))?;
        let db = Database::open()
            .await
            .map_err(|e| format!("Database error: {e}"))?;
        let db = Arc::new(db);

        let auth = Arc::new(AuthService::new(AuthClientConfig {
            firebase_api_key: FIREBASE_API_KEY.to_string(),
            google_oauth_client_id: GOOGLE_OAUTH_CLIENT_ID.to_string(),
            google_oauth_client_secret: GOOGLE_OAUTH_CLIENT_SECRET.to_string(),
            microsoft_oauth_client_id: MICROSOFT_OAUTH_CLIENT_ID.to_string(),
        }));
        let api = Arc::new(ApiClient::new(config.server.environment, Arc::clone(&auth)));

        // Select storage backend based on config
        // TODO: Add S3 backend selection when China deployment is configured
        let storage = Arc::new(lw_core::storage::StorageBackend::Gcs(
            lw_core::storage::GcsBackend::new(),
        ));

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let upload_engine = Arc::new(UploadEngine::new(
            Arc::clone(&db),
            Arc::clone(&api),
            storage,
            event_tx,
            config.upload.auto_clean,
            config.desensitization.strip_metadata,
            config.transcode.clone(),
            config.upload.chunk_size_mb,
            config.upload.max_concurrent_uploads,
        ));

        // Spawn background auto-retry for failed uploads on network recovery
        upload_engine.spawn_auto_retry();

        Ok(Self {
            auth,
            api,
            db,
            upload_engine,
            config,
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
        })
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct AppState {
    pub is_authenticated: Signal<bool>,
    pub user_info: Signal<Option<UserInfo>>,
    pub selected_tenant: Signal<Option<Tenant>>,
    pub selected_project: Signal<Option<Project>>,
    pub upload_tasks: Signal<Vec<UploadTask>>,
    pub projects: Signal<Vec<Project>>,
    pub tenant_projects: Signal<HashMap<String, Vec<Project>>>,
    pub is_loading: Signal<bool>,
    pub error_message: Signal<Option<String>>,
    pub auth_token: Signal<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            is_authenticated: Signal::new(false),
            user_info: Signal::new(None),
            selected_tenant: Signal::new(None),
            selected_project: Signal::new(None),
            upload_tasks: Signal::new(Vec::new()),
            projects: Signal::new(Vec::new()),
            tenant_projects: Signal::new(HashMap::new()),
            is_loading: Signal::new(false),
            error_message: Signal::new(None),
            auth_token: Signal::new(String::new()),
        }
    }
}
