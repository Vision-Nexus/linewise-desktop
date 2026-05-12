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
    pub show_settings: Signal<bool>,
}

/// Current filter for the upload queue. Derived from `selected_tenant` +
/// `selected_project`, not stored directly, so the two signals stay the
/// single source of truth and the derived value can't drift.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// No tenant selected — show every task across every org.
    All,
    /// Tenant selected but no project — show every task for that tenant.
    Tenant { tenant_id: String },
    /// Fully qualified — show tasks for this (tenant, project) pair.
    Project {
        tenant_id: String,
        project_id: String,
    },
}

impl Scope {
    /// True when the given task belongs to this scope. Used to filter the
    /// upload queue view.
    pub fn matches(&self, task_tenant_id: &str, task_project_id: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Tenant { tenant_id } => tenant_id == task_tenant_id,
            Scope::Project {
                tenant_id,
                project_id,
            } => tenant_id == task_tenant_id && project_id == task_project_id,
        }
    }

    /// Only `Project` scope has enough context to stage new uploads; the
    /// engine needs both a tenant id AND a project id.
    pub fn is_uploadable(&self) -> bool {
        matches!(self, Scope::Project { .. })
    }
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
            show_settings: Signal::new(false),
        }
    }

    /// Resolve a tenant id to its human-readable display name. Falls back to
    /// the id itself when the tenant isn't in the user_info cache — graceful
    /// for cross-tenant queue rows whose tenant isn't the currently selected
    /// one.
    pub fn tenant_display_name(&self, tenant_id: &str) -> String {
        let found = self
            .user_info
            .read()
            .as_ref()
            .and_then(|u| u.tenants.iter().find(|t| t.id == tenant_id).cloned());
        match found {
            Some(t) => t.display_name,
            None => tenant_id.to_string(),
        }
    }

    /// Resolve a (tenant_id, project_id) pair to the project's display name.
    /// Searches `tenant_projects` first (populated on sidebar mount) and
    /// `projects` second. Falls back to the raw id when neither has been
    /// hydrated yet — e.g. a queued task belonging to a tenant the user
    /// hasn't selected this session.
    /// Derive the current upload-queue scope from the selected-tenant and
    /// selected-project signals. Reading `Scope` via this helper avoids the
    /// two-signal drift that used to drive the stale/empty-queue bug.
    pub fn scope(&self) -> Scope {
        let tenant_id = self
            .selected_tenant
            .read()
            .as_ref()
            .map(|t| t.id.clone());
        let project_id = self
            .selected_project
            .read()
            .as_ref()
            .map(|p| p.id.clone());
        match (tenant_id, project_id) {
            (None, _) => Scope::All,
            (Some(tenant_id), None) => Scope::Tenant { tenant_id },
            (Some(tenant_id), Some(project_id)) => Scope::Project {
                tenant_id,
                project_id,
            },
        }
    }

    pub fn project_display_name(&self, tenant_id: &str, project_id: &str) -> String {
        if let Some(projects) = self.tenant_projects.read().get(tenant_id)
            && let Some(project) = projects.iter().find(|p| p.id == project_id)
        {
            return project.name.clone();
        }
        if let Some(project) = self.projects.read().iter().find(|p| p.id == project_id) {
            return project.name.clone();
        }
        project_id.to_string()
    }
}
