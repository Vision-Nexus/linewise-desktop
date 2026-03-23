use dioxus::prelude::*;
use lw_core::api_client::ApiClient;
use lw_core::auth::AuthService;
use lw_core::config::AppConfig;
use lw_core::db::Database;
use lw_core::models::{Project, Tenant, UploadTask, UserInfo};
use lw_core::upload::{UploadEngine, UploadEvent};
use std::sync::Arc;
use tokio::sync::mpsc;

const FIREBASE_API_KEY: &str = "AIzaSyDqUP3c44v-S22hyPJdjSTCNAFai_-3914";

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
        let db = Database::open().await.map_err(|e| format!("Database error: {e}"))?;
        let db = Arc::new(db);

        let auth = Arc::new(AuthService::new(FIREBASE_API_KEY.to_string()));
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
            config.upload.chunk_size_mb,
        ));

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
    pub is_loading: Signal<bool>,
    pub error_message: Signal<Option<String>>,
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
            is_loading: Signal::new(false),
            error_message: Signal::new(None),
        }
    }
}
