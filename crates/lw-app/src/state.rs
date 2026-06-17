use dioxus::prelude::*;
use lw_core::api_client::ApiClient;
use lw_core::auth::{AuthClientConfig, AuthService};
use lw_core::config::AppConfig;
use lw_core::db::Database;
use lw_core::error::ConfigError;
use lw_core::models::{Project, Tenant, UploadTask, UserInfo};
use lw_core::upload::{UploadEngine, UploadEvent};
use lw_core::version_check::VersionStatus;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::task::JoinHandle;

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
    pub event_rx: Arc<TokioMutex<mpsc::UnboundedReceiver<UploadEvent>>>,
    /// Handle for the long-running auto-retry task spawned in `init()`.
    /// Wrapped in `Arc<Mutex<Option<…>>>` so the bundle stays `Clone`
    /// (Dioxus props need it) while still letting the Repair flow
    /// abort the worker before wiping the SQLite files. The task holds
    /// `Arc<UploadEngine>` and therefore `Arc<Database>`, so leaving it
    /// alive across `Database::reset_local_files` would let the pool
    /// recreate WAL/SHM sidecars mid-wipe.
    pub auto_retry_handle: Arc<TokioMutex<Option<JoinHandle<()>>>>,
}

/// Identity-based equality for Dioxus prop memoization. Two `CoreServices`
/// are "equal" iff they share the same underlying Arc handles — i.e. they
/// come from the same `init()` call. After a reset-and-retry the new
/// services have fresh Arcs, so `PartialEq` returns false and Dioxus
/// remounts the dependent subtree.
impl PartialEq for CoreServices {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.auth, &other.auth)
            && Arc::ptr_eq(&self.api, &other.api)
            && Arc::ptr_eq(&self.db, &other.db)
            && Arc::ptr_eq(&self.upload_engine, &other.upload_engine)
            && Arc::ptr_eq(&self.event_rx, &other.event_rx)
    }
}

impl CoreServices {
    #[tracing::instrument(skip_all)]
    pub async fn init() -> Result<Self, String> {
        let config = AppConfig::load().map_err(|e| {
            tracing::warn!(error = %e, "core services init: config load failed");
            format!("Config error: {e}")
        })?;
        let db = Database::open().await.map_err(|e| {
            tracing::warn!(error = %e, "core services init: database open failed");
            format!("Database error: {e}")
        })?;
        let db = Arc::new(db);

        // Optional fixed proxy (e.g. v2ray's HTTP inbound) shared by all
        // three HTTP clients. Captured once here at startup — clients build
        // once and live for the session, so a config change takes effect on
        // next launch (the settings UI says so). `as_deref()` passes the
        // borrowed &str into each constructor.
        let proxy_url = config.server.proxy_url.clone();
        let auth = Arc::new(AuthService::new(AuthClientConfig {
            firebase_api_key: FIREBASE_API_KEY.to_string(),
            google_oauth_client_id: GOOGLE_OAUTH_CLIENT_ID.to_string(),
            google_oauth_client_secret: GOOGLE_OAUTH_CLIENT_SECRET.to_string(),
            microsoft_oauth_client_id: MICROSOFT_OAUTH_CLIENT_ID.to_string(),
            proxy_url: proxy_url.clone(),
        }));
        let api = Arc::new(ApiClient::new(
            config.server.environment,
            Arc::clone(&auth),
            proxy_url.as_deref(),
        ));

        // Select storage backend based on config
        // TODO: Add S3 backend selection when China deployment is configured
        let storage = Arc::new(lw_core::storage::StorageBackend::Gcs(
            lw_core::storage::GcsBackend::new(proxy_url.as_deref()),
        ));

        // Video quality rules now live on the server. The desktop ships
        // the head bytes and the API returns the verdict; nothing to
        // load at startup any more.
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
            config.upload.sequential_uploads,
        ));

        // Spawn background auto-retry for failed uploads on network
        // recovery. We hold onto the JoinHandle so the Repair flow can
        // abort the worker before wiping the SQLite files — see the
        // doc comment on `auto_retry_handle`.
        let auto_retry = upload_engine.spawn_auto_retry();

        tracing::info!("core services ready");
        Ok(Self {
            auth,
            api,
            db,
            upload_engine,
            config,
            event_rx: Arc::new(TokioMutex::new(event_rx)),
            auto_retry_handle: Arc::new(TokioMutex::new(Some(auto_retry))),
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
    /// Per-task progress maps, written by the resident `UploadRuntime`'s
    /// single event pump and read by the transfer view. Kept out of
    /// `upload_tasks` so a byte-level `Progress` tick doesn't churn the
    /// whole task list. `upload_progress` is clamped monotonic in the
    /// event handler. `Signal<T>` is `Copy`, so views take cheap handles.
    pub transcode_progress: Signal<HashMap<String, f32>>,
    pub upload_progress: Signal<HashMap<String, (u64, u64)>>,
    pub hash_progress: Signal<HashMap<String, (u64, u64)>>,
    pub projects: Signal<Vec<Project>>,
    pub tenant_projects: Signal<HashMap<String, Vec<Project>>>,
    pub is_loading: Signal<bool>,
    pub error_message: Signal<Option<String>>,
    pub auth_token: Signal<String>,
    pub show_settings: Signal<bool>,
    /// Title-bar Repair affordance: when true, the repair modal renders
    /// over the app shell. Reachable independent of auth state — a wedged
    /// app needs this even when sign-in itself is failing.
    pub show_repair: Signal<bool>,
    pub toast: Signal<Option<Toast>>,
    pub services: Signal<Option<CoreServices>>,
    /// Live, in-memory `AppConfig`. Settings panes write through
    /// `save_config` which both persists to disk and updates this
    /// signal in one step, so any reader (e.g. the upload queue's
    /// per-task transcode toggle) re-renders on change. The boot
    /// effect populates this from disk on startup. Treat it as the
    /// source of truth for any config field that affects UI; the
    /// `CoreServices.config` field is a frozen snapshot used only by
    /// services constructed at init time (ApiClient, UploadEngine).
    pub config: Signal<AppConfig>,
    /// Monotonic counter the root `App` watches; bump it from anywhere
    /// in the tree to ask the bootstrap effect to rebuild
    /// `CoreServices`. Used by the environment switcher in settings —
    /// the new `ApiClient`'s base URL is read from `AppConfig` at init
    /// time, so the only way to switch hosts cleanly is a re-init.
    pub restart_token: Signal<u64>,
    /// Result of the startup version check against GitHub. `None` means
    /// the check hasn't completed yet, or it failed and we treat the
    /// status as unknown — both are non-blocking. `Some(Unsupported {..})`
    /// is the only state that gates rendering.
    pub version_status: Signal<Option<VersionStatus>>,
}

/// Lightweight toast notification. Only one toast lives at a time — a
/// fresh `show_toast` call replaces any in-flight toast, which keeps the
/// overlay simple and matches the behaviour users expect (a save ack
/// never lines up behind an unrelated info message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    /// Monotonic counter so the overlay's auto-dismiss can tell "the
    /// toast I was asked to dismiss" from "a newer toast that replaced
    /// it while I was sleeping".
    pub id: u64,
    pub message: String,
    pub kind: ToastKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToastKind {
    Success,
    Error,
    Info,
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
    /// Only `Project` scope has enough context to stage new uploads; the
    /// engine needs both a tenant id AND a project id.
    ///
    /// The transfer panel renders globally (every org at once) and narrows
    /// to the selected project through its own opt-in filter, so `Scope` no
    /// longer drives a per-row view filter — it only gates whether the
    /// current selection can be an upload *target*.
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
            transcode_progress: Signal::new(HashMap::new()),
            upload_progress: Signal::new(HashMap::new()),
            hash_progress: Signal::new(HashMap::new()),
            projects: Signal::new(Vec::new()),
            tenant_projects: Signal::new(HashMap::new()),
            is_loading: Signal::new(false),
            error_message: Signal::new(None),
            auth_token: Signal::new(String::new()),
            show_settings: Signal::new(false),
            show_repair: Signal::new(false),
            toast: Signal::new(None),
            services: Signal::new(None),
            config: Signal::new(AppConfig::default()),
            restart_token: Signal::new(0),
            version_status: Signal::new(None),
        }
    }

    /// Persist `next` to `config.toml` and update the live signal in
    /// one step. Settings panes call this from their Save handler
    /// instead of `AppConfig::load -> mutate -> save`. Returns the
    /// disk error so the pane can decide how to surface it (toast).
    /// On error, the signal is not touched — the on-disk and in-memory
    /// states stay coherent.
    #[tracing::instrument(skip_all)]
    pub fn save_config(&mut self, next: AppConfig) -> Result<(), ConfigError> {
        next.save()?;
        self.config.set(next);
        tracing::info!("config saved");
        Ok(())
    }

    /// Ask the bootstrap effect to re-run `CoreServices::init()`. The
    /// next signal read inside the boot future flips back to
    /// `Initializing`, which rebuilds the `ApiClient` against whatever
    /// environment is in `AppConfig` at that moment. Callers should
    /// persist their config change first.
    pub fn request_restart(&mut self) {
        let next = self.restart_token.peek().wrapping_add(1);
        self.restart_token.set(next);
    }

    /// Publish a toast. Replaces any currently-visible toast.
    ///
    /// Auto-dismiss is owned by `ToastOverlay`, not this method. The
    /// overlay sits at the root of the component tree and watches the
    /// toast signal; it (re)schedules a dismiss task in its own scope
    /// every time the toast id changes. Doing it here would tie the
    /// dismiss task to the *caller's* scope, which would get cancelled
    /// when components like the environment switcher trigger a remount
    /// via `request_restart` — and the toast would stick.
    pub fn show_toast(&mut self, message: impl Into<String>, kind: ToastKind) {
        let next_id = self
            .toast
            .read()
            .as_ref()
            .map(|t| t.id.wrapping_add(1))
            .unwrap_or(1);
        self.toast.set(Some(Toast {
            id: next_id,
            message: message.into(),
            kind,
        }));
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
        let tenant_id = self.selected_tenant.read().as_ref().map(|t| t.id.clone());
        let project_id = self.selected_project.read().as_ref().map(|p| p.id.clone());
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
