use crate::api_client::ApiClient;
use crate::config::TranscodeConfig;
use crate::container_kind::{self, ContainerKind};
use crate::db::Database;
use crate::dedup;
use crate::error::{DbError, UploadError, VideoValidationError};
use crate::ffmpeg_util;
use crate::models::{
    Acceptance, CreateDocumentMeta, CreateDocumentRequest, Digest, DigestCheckCandidate,
    NearDuplicateMatch, PdqFrameWire, Tenant, UploadState, UploadTask,
};
use crate::pdq;
use crate::storage::{self, StorageBackend};
use crate::transcode;
use crate::video;
use crate::video_head;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{OnceCell, Semaphore, mpsc};
use uuid::Uuid;

/// Classify a file path as video by sniffing its MIME type from the
/// extension. Lifted out of `stage_file` so the UI can pre-filter
/// dropped files at drag-and-drop time without re-implementing the
/// mime_guess lookup.
pub fn looks_like_video(path: &Path) -> bool {
    mime_guess::from_path(path).first_or_octet_stream().type_() == mime_guess::mime::VIDEO
}

/// Recursively walk `dir` and return every video file underneath it, filtered
/// by [`looks_like_video`]. Non-video files and entries that can't be read are
/// skipped silently — the caller decides whether the resulting list is empty
/// and surfaces that to the user. Symlinks are NOT followed, so a cyclic link
/// can't trap the walk in an infinite loop.
///
/// This is the single folder-recursion entry shared by the sidebar Upload
/// button, the right-click project picker, the transfer-panel header button,
/// and folder drag-drop. It is synchronous and walks with `std::fs`; callers
/// MUST run it off the UI thread (`tokio::task::spawn_blocking`) because a
/// deep directory tree on a slow disk would otherwise block the renderer.
///
/// If `dir` is not a directory the returned vec is empty (a plain file dropped
/// or picked is handled by the per-file staging path, not this walk).
pub fn collect_videos_in_dir(dir: &Path) -> Vec<PathBuf> {
    let mut videos = Vec::new();
    collect_videos_into(dir, &mut videos);
    videos
}

/// Depth-first worker for [`collect_videos_in_dir`]. Errors reading a
/// directory entry are logged at `debug` and skipped rather than aborting the
/// whole walk, so one unreadable subfolder doesn't lose the rest of the tree.
fn collect_videos_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), "collect_videos: read_dir failed: {e}");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Use the dir entry's own file-type rather than `path.is_dir()` so we
        // don't follow symlinks (the entry type reports the link itself).
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_videos_into(&path, out);
        } else if file_type.is_file() && looks_like_video(&path) {
            out.push(path);
        }
    }
}

/// Deferred verdict emitted by the staging-time quality check. The hash
/// worker reads this after BLAKE3+MD5 finishes and routes the row to
/// `Staged` or `Rejected` accordingly. We hash quality-rejected rows
/// anyway so a super-admin Force-upload bypass has the MD5 ready to
/// send to `create_document` / `dedup-checks`.
enum PostHashVerdict {
    Stage,
    Reject(Vec<String>),
}

/// Outcome of the cross-tenant dedup check. Three-way to model the
/// "resume an abandoned same-user upload" branch:
///
///   * `Allow` — no match, or all matches are someone else's; staging
///     proceeds and `process_task` will create a fresh document.
///   * `Reuse(document_id)` — a tenant match exists whose
///     `creator_id` is the current user AND whose `gcs_uri` is null
///     (the document row was created but the blob was never
///     uploaded). Staging stamps the task with this id so Stage 4
///     skips `create_document` and the existing resumable-upload
///     machinery picks up where the abandoned attempt left off.
///   * `Reject(reason)` — the match is real and not recoverable
///     by reuse; the row goes to `Rejected` with `reason` shown to
///     the user.
enum DedupVerdict {
    Allow,
    Reuse(String),
    Reject(String),
}

/// Outcome of the mandatory pre-create dedup gate (`precreate_dedup`), run
/// immediately before every `create_document`. Stops the create call from
/// minting a second document for content that already has one.
enum PreCreate {
    /// No duplicate — create a new document.
    Create,
    /// An existing document for this content was found and is safe to reuse:
    /// the in-flight orphan a lost create-response left behind, or an abandoned
    /// same-user upload (gcs_uri still null). Adopt its id and upload to it.
    Adopt(String),
    /// A concurrent sibling task — possibly in a second app instance sharing
    /// this SQLite DB — is already uploading this exact content. Defer to it.
    AlreadyInFlight { existing_id: String },
    /// A duplicate already exists in the tenant (completed, cross-tenant, or
    /// perceptual). Settle `Rejected` with this reason instead of creating.
    Rejected(String),
}

/// Render a user-facing rejection line for a PDQ near-duplicate hit. Mirrors
/// the exact-dedup "already uploaded" wording but flags the perceptual nature
/// so the user understands it's a re-encode / re-mux of footage already in the
/// tenant, not a byte-identical file.
fn near_duplicate_message(near: &NearDuplicateMatch) -> String {
    let pct = (near.coverage * 100.0).round() as i64;
    let plural = if near.matched_frames == 1 { "" } else { "s" };
    format!(
        "Near-duplicate of an existing video in this tenant ({} frame{plural} matched, {pct}% coverage)",
        near.matched_frames,
    )
}

/// Coarse connectivity tier reported by the auto-retry loop's periodic probe,
/// classified from the round-trip time of a HEAD to `storage.googleapis.com`
/// (through the same proxy-aware client the uploads use). A "game ping"-style
/// signal for the UI: the exact millisecond count is advisory, the tier drives
/// the indicator colour and the weak-network prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkHealth {
    /// Probe succeeded quickly — storage is comfortably reachable.
    Good,
    /// Probe succeeded but slow — uploads will work, just not fast.
    Ok,
    /// Probe succeeded very slowly, or is failing intermittently within the
    /// window — uploads will struggle. This is the "weak network" tier that
    /// drives the prompts.
    Weak,
    /// Probe has failed on consecutive ticks — storage is unreachable.
    Offline,
}

impl NetworkHealth {
    /// Whether this tier is the "weak network" band that drives the prompts —
    /// [`NetworkHealth::Weak`] or [`NetworkHealth::Offline`]. `Good`/`Ok` are
    /// healthy. Exposed so the UI banner and chip share one definition.
    pub fn is_weak(self) -> bool {
        match self {
            Self::Good | Self::Ok => false,
            Self::Weak | Self::Offline => true,
        }
    }

    /// This tier floored at [`NetworkHealth::Weak`]: `Good`/`Ok` degrade to
    /// `Weak`, while the already-worse `Weak`/`Offline` pass through unchanged.
    ///
    /// The transfer-panel chip uses this so a green PROBE reading cannot show a
    /// healthy tier while actual part PUTs are failing and retrying — a case
    /// proven live (a lightweight HEAD probe to storage reads Good while 64 MiB
    /// part PUTs fail through a flaky proxy). The chip takes the worse of the
    /// probe tier and this floor whenever any part is retrying.
    pub fn at_least_weak(self) -> Self {
        match self {
            Self::Good | Self::Ok | Self::Weak => Self::Weak,
            Self::Offline => Self::Offline,
        }
    }
}

/// One connectivity reading emitted to the UI. `rtt_ms` is the probe
/// round-trip in milliseconds when the probe succeeded, `None` when it failed
/// (tier is then [`NetworkHealth::Weak`] or [`NetworkHealth::Offline`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkReading {
    pub health: NetworkHealth,
    pub rtt_ms: Option<u32>,
}

/// Events emitted by the upload engine to the UI
#[derive(Debug, Clone)]
pub enum UploadEvent {
    /// A connectivity-tier change observed by the auto-retry loop's periodic
    /// probe. Emitted only on a tier transition (debounced), so the UI's
    /// signal-strength chip and weak-network banner update on change rather
    /// than every 30s tick. See [`NetworkReading`].
    NetworkQuality(NetworkReading),
    TaskAdded(Box<UploadTask>),
    StateChanged {
        task_id: String,
        state: UploadState,
    },
    Progress {
        task_id: String,
        bytes_uploaded: u64,
        total_bytes: u64,
    },
    /// Hashing progress for a row in the `Hashing` state. Coalesced
    /// to ~1 MiB granularity by the hasher; the UI shows a determinate
    /// progress bar and switches the row out of "Hashing" once
    /// `StateChanged → Staged | Rejected` arrives.
    HashProgress {
        task_id: String,
        bytes_hashed: u64,
        total_bytes: u64,
    },
    /// Save-time capture-embed progress for a `Staged` row. Derived by polling
    /// exiftool's `*_exiftool_tmp` rewrite file size against the source size, so
    /// the UI shows a determinate bar during the (possibly multi-GB) in-place
    /// rewrite. A final event with `bytes == total` signals completion; the UI
    /// then drops the bar.
    CaptureEmbedProgress {
        task_id: String,
        bytes: u64,
        total: u64,
    },
    /// Replace the per-row hint lists in the UI cache. `warnings`
    /// renders in the warn palette, `rejection_reasons` in the error
    /// palette. Either may be empty; both are sent together so the
    /// UI never observes a half-updated row.
    ValidationWarnings {
        task_id: String,
        warnings: Vec<String>,
        rejection_reasons: Vec<String>,
    },
    /// The server quality check finished with a usable verdict
    /// (`Accepted` or `Rejected`). Carries the populated `video_info`
    /// and advisory `warnings` so the UI's per-row popover and
    /// warn-palette lines can render before the hash worker takes
    /// over. A subsequent `StateChanged → Hashing` flips the row out
    /// of the `QualityChecking` section. The fail path (broken file,
    /// unsupported container, server unreachable) does not emit this
    /// event — those settle directly through `ValidationWarnings`
    /// with a populated `rejection_reasons` and a `StateChanged →
    /// Rejected`, because the row carries no usable `video_info`.
    QualityCheckPassed {
        task_id: String,
        video_info: Option<crate::models::VideoInfo>,
        warnings: Vec<String>,
    },
    TranscodeProgress {
        task_id: String,
        percent: f32,
    },
    /// Emitted once when transcoding finishes and the final artifact size is
    /// known. The UI uses this to render "original → transcoded" bytes and to
    /// switch the progress label from "Transcoding" to "Uploading".
    TranscodeCompleted {
        task_id: String,
        transcoded_size: u64,
    },
    DuplicateDetected {
        task_id: String,
        existing_document_id: String,
    },
    Completed {
        task_id: String,
    },
    Failed {
        task_id: String,
        error: String,
    },
    /// A previously-failed upload is being auto-retried after network recovery.
    /// `attempt` is the 1-based auto-retry number. The UI shows it so a
    /// re-queued file reads as "retrying (attempt N)" instead of a silent
    /// PENDING/UPLOADING row.
    Retrying {
        task_id: String,
        attempt: u32,
    },
    /// An in-flight multipart part PUT failed a transport attempt and is backing
    /// off before the next try — emitted from the storage retry loop
    /// (`put_part_with_retry` → `retry_io`) via the driver's on-retry callback.
    /// `attempt` is the 1-based failed-attempt count for that part.
    ///
    /// This is the event-driven signal that replaces the old byte-progress
    /// timeout for the per-row "connection stalled" hint: a HEALTHY big-file
    /// upload can legitimately show no `Progress` for tens of seconds (the
    /// backend hands big files 64 MiB parts and MPU progress only fires per
    /// completed part), so silence is not a stall — an actual failing part
    /// retry is. The UI marks the row stalled on this event and clears it on the
    /// next `Progress` (a part landed) or any terminal/non-`Uploading`
    /// transition. Distinct from [`UploadEvent::Retrying`], which is the
    /// whole-task auto-retry after a give-up, not a within-attempt part retry.
    PartRetrying {
        task_id: String,
        attempt: u32,
    },
}

/// Cap on how many files run their staging-time quality check + hash stream
/// concurrently. Staging 100 files at once otherwise spawns 100 background
/// workers that each open a server `/quality-check` round-trip and a streaming
/// BLAKE3+MD5 read — flooding the event channel and saturating the network.
/// Throttling the QC/hash churn to a trickle keeps the row inserts (and their
/// `TaskAdded` events) instant while the heavy work drains a few at a time.
/// Uploads themselves are bounded separately by `upload_semaphore`.
const MAX_CONCURRENT_STAGING: usize = 4;

/// RAII guard that deletes the transcode temp copy on EVERY exit of
/// `process_task` — success, an `Err` propagated via `?`, or a panic/cancel —
/// not only the success tail (`finalize_after_upload`). Before this, any failure
/// between transcode and finalize leaked a full-size copy that accumulated until
/// the disk filled (SQLite "(code 13) database or disk is full"). Deleting on a
/// resumable `Err` is safe: the temp copy is never persisted across runs — a
/// retried/resumed task rebuilds it from the unchanged source (transcode resumes
/// from its own HLS scratch dir, which this guard deliberately does NOT track).
/// `Drop` only removes a path that still exists, so on the success path — where
/// `finalize_after_upload` already cleaned the copy — it is a silent no-op (no
/// double-delete warning).
#[derive(Default)]
struct TempCleanupGuard {
    paths: Vec<PathBuf>,
}

impl TempCleanupGuard {
    /// Register a produced temp copy for cleanup-on-exit. `None` is ignored.
    fn track(&mut self, path: Option<&Path>) {
        if let Some(path) = path {
            self.paths.push(path.to_path_buf());
        }
    }
}

impl Drop for TempCleanupGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            if path.exists() {
                ffmpeg_util::cleanup_temp_file(path);
            }
        }
    }
}

pub struct UploadEngine {
    db: Arc<Database>,
    api: Arc<ApiClient>,
    storage: Arc<StorageBackend>,
    event_tx: mpsc::UnboundedSender<UploadEvent>,
    /// Dedicated client for the auto-retry loop's liveness/health probe, built
    /// once from `ServerConfig::proxy_url` so the probe follows the same path as
    /// the uploads (a proxied user's probe tunnels through v2ray too, instead of
    /// hitting `storage.googleapis.com` directly and reporting a false "online"
    /// while the direct route to GCS is what's actually wedged). Short connect +
    /// total timeouts keep the probe snappy; it never carries a request body.
    probe_client: reqwest::Client,
    /// Whether to delete the original file on disk after a successful upload.
    /// Flipped live from the settings UI; reads are `Relaxed` because each
    /// upload task reads this exactly once, after the upload has already
    /// completed, so ordering against other work is irrelevant.
    auto_clean: AtomicBool,
    transcode_config: TranscodeConfig,
    chunk_size: u64,
    upload_semaphore: Arc<Semaphore>,
    /// Caps concurrent staging-time quality-check + hash work at
    /// [`MAX_CONCURRENT_STAGING`]. Held by each `stage_file` background worker
    /// across its QC + hash phase only; the synchronous row insert + `TaskAdded`
    /// emit run unbounded so every dropped file appears in the queue at once.
    stage_semaphore: Arc<Semaphore>,
    /// Lazy-cached `whoami` snapshot for the logged-in user, populated
    /// on the first dedup check that needs it. The engine is built
    /// before login, so we can't pass it through the constructor; the
    /// API client is already authenticated by the time staging runs,
    /// so a one-shot `whoami` round-trip is fine. Failure is
    /// non-fatal — same-user reuse and cross-tenant naming both fall
    /// back to less-rich behaviour, mirroring how a transient API
    /// outage is handled.
    current_user_cache: OnceCell<CurrentUserCache>,
    /// User-entered io.visionlab capture metadata, keyed by task id. In-memory
    /// only (not persisted) — the UI calls [`Self::set_capture_metadata`] when
    /// the form is saved; `process_task` reads it at Stage 0 and embeds it into
    /// the file before hashing/upload. Held on the engine (not the `UploadTask`
    /// row) so it survives the DB-reload that the manual-confirm dispatch path
    /// (`confirm_staged`) performs. Lost on app restart (acceptable for v1).
    capture_metadata:
        std::sync::Mutex<std::collections::HashMap<String, crate::capture::CaptureMetadata>>,
    /// Current batch-default capture metadata, applied to every file staged while
    /// it is set ("set defaults → add files" UX): a staged file with a batch
    /// default in effect gets a per-file entry at stage time, so it shows
    /// "✓ filled" and uploads on the next manual "Upload" without a per-file fill.
    /// In-memory only. `None` = no default; files then hold `Staged` showing
    /// "Needs metadata" until the fill UI sets it (capture is required — the
    /// manual `confirm_staged` skips any clip still missing metadata).
    batch_capture: std::sync::Mutex<Option<crate::capture::CaptureMetadata>>,
    /// Task ids whose SOURCE FILE already carries the io.visionlab tags — either
    /// the Save-time in-place embed succeeded, or the tags were read back from the
    /// file at stage time (re-add / vendor-pre-tagged). Upload Stage 0 skips
    /// re-embedding these (the rewrite already happened), avoiding a second
    /// multi-GB pass. In-memory only.
    capture_embedded: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Task ids whose required capture metadata the user chose to SKIP. Capture is
    /// offered but not mandatory: a skipped clip carries no metadata yet counts as
    /// "resolved" ([`Self::capture_resolved`]) so it auto-advances to upload. Held
    /// alongside `capture_metadata` (in-memory only, lost on restart) and mutually
    /// exclusive with it — filling a clip clears its skip, and skipping clears any
    /// recorded metadata. Unlike a filled clip, a skipped clip embeds NO tags, so
    /// on restart it falls back to "Needs metadata" and the user is re-offered the
    /// choice (acceptable for v1, mirroring the in-memory `capture_metadata`).
    capture_skipped: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Process-local single-flight set: the ids of tasks a worker is currently
    /// driving. A dispatch/resume/retry whose id is already present is dropped,
    /// so one task id can never be processed by two concurrent workers in this
    /// process (closes the `auto_advance`-vs-`auto_advance`, force-on-in-flight,
    /// and resume-vs-retry double-run classes). This is the authoritative
    /// in-process guard; the single-instance guard (`lw-app/single_instance.rs`)
    /// covers the cross-process case, and the guarded terminal settles
    /// (`db::settle_completed`/`settle_failure`) make even a degraded double-run
    /// non-corrupting. Entries are removed on drop of the [`InFlightGuard`].
    in_flight: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Per-task cooperative cancel flags, keyed by task id. [`Self::pause_task`]
    /// trips a flag; the upload stage (`storage::upload_file_chunked` /
    /// `upload_file_mpu`) polls it at each chunk/part boundary and returns
    /// [`UploadError::Cancelled`], which the dispatch layer settles to `Paused`.
    /// Populated alongside the single-flight entry in [`Self::try_enter_flight`]
    /// and removed on [`InFlightGuard`] drop, so a flag exists exactly while a
    /// worker owns the id. Only the `Uploading` stage polls it — pausing any other
    /// stage is meaningless (see `state_machine`: only `Uploading -> Paused`).
    cancels: Arc<
        std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    >,
}

/// RAII membership in [`UploadEngine::in_flight`]. Holding one means "this worker
/// owns processing of `id` in this process"; dropping it releases the id. Created
/// only via [`UploadEngine::try_enter_flight`], which returns `None` when the id
/// is already in flight.
struct InFlightGuard {
    set: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    cancels: Arc<
        std::sync::Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    >,
    id: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set
            .lock()
            .expect("in_flight lock poisoned")
            .remove(&self.id);
        // Drop the cancel flag too, so a paused-then-resumed dispatch mints a
        // fresh (untripped) flag rather than inheriting a stale `true`.
        self.cancels
            .lock()
            .expect("cancels lock poisoned")
            .remove(&self.id);
    }
}

/// What the dedup gate needs from the logged-in user: their Linewise
/// UserId (matched against `creatorId` for the reuse branch) and the
/// list of tenants they belong to (used to render display names in
/// the cross-tenant rejection message).
struct CurrentUserCache {
    user_id: String,
    tenants: Vec<Tenant>,
}

impl UploadEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<Database>,
        api: Arc<ApiClient>,
        storage: Arc<StorageBackend>,
        event_tx: mpsc::UnboundedSender<UploadEvent>,
        auto_clean: bool,
        transcode_config: TranscodeConfig,
        chunk_size_mb: u32,
        max_concurrent: u32,
        proxy: Option<&str>,
    ) -> Self {
        // Probe client: proxy-aware (so the health check matches the upload
        // path) with tight timeouts — a probe should resolve in seconds, and a
        // dead/wrong proxy must fail fast rather than stall the 30s tick loop.
        // A bad proxy URL degrades to no-proxy inside `build_http_client`
        // rather than panicking, so a settings typo can never brick the engine.
        let probe_client = crate::net::build_http_client(
            proxy,
            Some(std::time::Duration::from_secs(5)),
            std::time::Duration::from_secs(5),
            None,
        )
        .expect("failed to build probe reqwest client");
        Self {
            db,
            api,
            storage,
            event_tx,
            probe_client,
            auto_clean: AtomicBool::new(auto_clean),
            transcode_config,
            chunk_size: (chunk_size_mb as u64) * 1024 * 1024,
            upload_semaphore: Arc::new(Semaphore::new(max_concurrent as usize)),
            stage_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_STAGING)),
            current_user_cache: OnceCell::new(),
            capture_metadata: std::sync::Mutex::new(std::collections::HashMap::new()),
            batch_capture: std::sync::Mutex::new(None),
            capture_embedded: std::sync::Mutex::new(std::collections::HashSet::new()),
            capture_skipped: std::sync::Mutex::new(std::collections::HashSet::new()),
            in_flight: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            cancels: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Try to claim in-process ownership of `task_id`. Returns a guard that
    /// releases the id on drop, or `None` if another worker in this process is
    /// already driving it (the caller should then drop the dispatch). This is the
    /// single-flight primitive that funnels every dispatch path through one
    /// worker per id — see the `in_flight` field.
    fn try_enter_flight(&self, task_id: &str) -> Option<InFlightGuard> {
        let newly_inserted = self
            .in_flight
            .lock()
            .expect("in_flight lock poisoned")
            .insert(task_id.to_string());
        if !newly_inserted {
            return None;
        }
        // Mint this worker's cooperative cancel flag alongside its single-flight
        // entry so `pause_task` can trip it while the worker runs; the guard drops
        // both on worker exit.
        self.cancels.lock().expect("cancels lock poisoned").insert(
            task_id.to_string(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        Some(InFlightGuard {
            set: Arc::clone(&self.in_flight),
            cancels: Arc::clone(&self.cancels),
            id: task_id.to_string(),
        })
    }

    /// The cooperative cancel flag for a running task, or a fresh never-tripped
    /// flag if none is registered (the worker isn't in flight). The upload stage
    /// polls the returned flag at each chunk/part boundary; an unregistered id
    /// yields a flag that is always `false`, i.e. no cancellation.
    fn cancel_flag_for(&self, task_id: &str) -> Arc<std::sync::atomic::AtomicBool> {
        self.cancels
            .lock()
            .expect("cancels lock poisoned")
            .get(task_id)
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false)))
    }

    /// Whether the document's blob is already durable on the server (has a
    /// `gcs_uri`). The resume paths call this before re-initiating an upload so a
    /// file that actually finished — whose MPU / resumable-session handle was
    /// merely reaped or expired — is finalized instead of re-uploaded from byte 0.
    /// A lookup failure is treated as "not known complete", falling back to the
    /// safe re-upload path.
    async fn document_blob_complete(
        &self,
        tenant_id: &str,
        project_id: &str,
        doc_id: &str,
    ) -> bool {
        match self.api.get_document(tenant_id, project_id, doc_id).await {
            Ok(doc) => doc.gcs_uri.is_some(),
            Err(e) => {
                tracing::warn!(doc_id, "resume: get_document failed, will re-upload: {e}");
                false
            }
        }
    }

    /// Set (or clear, with `None`) the capture metadata the UI collected for a
    /// specific task (per-file override). Read by `process_task` Stage 0. Setting
    /// metadata clears any prior "skipped" mark for the task — the two are mutually
    /// exclusive resolutions (see [`Self::capture_resolved`]).
    pub fn set_capture_metadata(
        &self,
        task_id: &str,
        meta: Option<crate::capture::CaptureMetadata>,
    ) {
        let mut map = self.capture_metadata.lock().expect("capture_metadata lock");
        match meta {
            Some(m) => {
                map.insert(task_id.to_string(), m);
                self.capture_skipped
                    .lock()
                    .expect("capture_skipped lock")
                    .remove(task_id);
            }
            None => {
                map.remove(task_id);
            }
        }
    }

    /// Mark a clip's required capture metadata as intentionally SKIPPED. Capture is
    /// offered but optional: a skipped clip records no metadata yet counts as
    /// "resolved" so it auto-advances to upload like a filled one. Clears any
    /// recorded metadata for the task (skip and fill are mutually exclusive). The
    /// caller is responsible for triggering [`Self::auto_advance_if_resolved`] after
    /// this so the now-resolved clip uploads without a manual click.
    pub fn skip_capture_metadata(&self, task_id: &str) {
        self.capture_metadata
            .lock()
            .expect("capture_metadata lock")
            .remove(task_id);
        self.capture_skipped
            .lock()
            .expect("capture_skipped lock")
            .insert(task_id.to_string());
    }

    /// Whether the user explicitly skipped capture metadata for `task_id`.
    pub fn is_capture_skipped(&self, task_id: &str) -> bool {
        self.capture_skipped
            .lock()
            .expect("capture_skipped lock")
            .contains(task_id)
    }

    /// UI entrypoint for "Skip" on a staged clip: mark its capture metadata as
    /// skipped, then auto-advance it to upload (the skip resolves the metadata
    /// gate). A transcode-eligible clip is still held `Staged` for the opt-in
    /// transcode confirm. Mirrors how [`Self::embed_capture_in_place`] bundles
    /// fill-then-advance, so both resolutions share the auto-advance path.
    pub async fn skip_capture_and_advance(self: &Arc<Self>, task_id: &str) {
        self.skip_capture_metadata(task_id);
        // Persist the skip so it survives a restart (otherwise the clip falls back
        // to "Needs metadata" and the user is re-prompted).
        if let Err(e) = self.db.set_capture_row(task_id, "skipped", None).await {
            tracing::warn!(task_id, "failed to persist capture skip: {e}");
        }
        self.auto_advance_if_resolved(task_id).await;
    }

    /// Whether a clip's required capture metadata is RESOLVED — either filled
    /// ([`Self::has_capture_metadata`]) or explicitly skipped
    /// ([`Self::skip_capture_metadata`]). A resolved `Staged` clip auto-advances to
    /// upload (unless held for transcode); an unresolved one holds `Staged` showing
    /// "Needs metadata" until the user fills or skips it.
    pub fn capture_resolved(&self, task_id: &str) -> bool {
        self.has_capture_metadata(task_id) || self.is_capture_skipped(task_id)
    }

    /// Set the batch-default capture metadata applied to every file staged from
    /// now on ("set defaults → add files"). `None` clears it. In-memory only.
    pub fn set_batch_capture_metadata(&self, meta: Option<crate::capture::CaptureMetadata>) {
        *self.batch_capture.lock().expect("batch_capture lock") = meta;
    }

    /// The current batch-default capture metadata, for the UI to show/edit.
    pub fn batch_capture_metadata(&self) -> Option<crate::capture::CaptureMetadata> {
        self.batch_capture
            .lock()
            .expect("batch_capture lock")
            .clone()
    }

    /// Whether per-file capture metadata is set for `task_id`. The manual
    /// `confirm_staged` requires this before dispatching a `Staged` clip, so a
    /// clip with no metadata (no batch default applied at stage time, no per-file
    /// entry) stays `Staged` until the UI fills it. Also drives the row's
    /// "Needs metadata" badge vs the "✓ filled" line.
    pub fn has_capture_metadata(&self, task_id: &str) -> bool {
        self.capture_metadata
            .lock()
            .expect("capture_metadata lock")
            .contains_key(task_id)
    }

    /// The per-file capture metadata recorded for `task_id`, if any — for the
    /// fill UI to prefill when re-editing a clip that already has values.
    pub fn capture_metadata_for(&self, task_id: &str) -> Option<crate::capture::CaptureMetadata> {
        self.capture_metadata
            .lock()
            .expect("capture_metadata lock")
            .get(task_id)
            .cloned()
    }

    /// Save-time embed for one clip: record `meta` AND write the io.visionlab tags
    /// into the source file in place, so the file is self-describing (a re-add
    /// reads them back) and the staging hash is invalidated for re-derivation.
    ///
    /// Returns `Ok(true)` when the source file was tagged in place; `Ok(false)`
    /// when the source couldn't be written (read-only / full removable media) —
    /// the values are still recorded in memory and the upload path's adaptive
    /// embed will tag a local copy instead. `Err` only when exiftool is missing.
    /// The full-file rewrite is a blocking multi-second op for large clips.
    pub async fn embed_capture_in_place(
        self: &Arc<Self>,
        task_id: &str,
        meta: crate::capture::CaptureMetadata,
    ) -> Result<bool, UploadError> {
        self.set_capture_metadata(task_id, Some(meta.clone()));
        // Serialize now, before `meta` is moved into the blocking embed closure,
        // so the resolution can be persisted to the row regardless of embed outcome.
        let meta_json = serde_json::to_string(&meta).ok();
        let Some(task) = self.db.get_upload_by_id(task_id).await? else {
            return Ok(false);
        };
        let input = PathBuf::from(&task.local_path);
        let total = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);

        // Determinate progress: exiftool writes `<input>_exiftool_tmp`, growing
        // linearly to the source size. We tick from a `select!` loop driven by the
        // SAME future as the blocking embed — when the embed completes we break and
        // emit exactly one final event. No separate racing task to abort, so a late
        // tick can never land after completion and resurrect a stale bar.
        let mut tmp_os = input.clone().into_os_string();
        tmp_os.push("_exiftool_tmp");
        let tmp_path = PathBuf::from(tmp_os);

        let embed_input = input.clone();
        let mut embed = tokio::task::spawn_blocking(move || {
            crate::capture::embed_in_place_blocking(&embed_input, &meta)
        });
        let res = loop {
            tokio::select! {
                joined = &mut embed => break joined.expect("capture in-place embed task panicked"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                    if total > 0 {
                        let bytes = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
                        let _ = self.event_tx.send(UploadEvent::CaptureEmbedProgress {
                            task_id: task_id.to_string(),
                            bytes: bytes.min(total),
                            total,
                        });
                    }
                }
            }
        };
        // Exactly one final event: full bar on success, clear on failure — the UI
        // drops the bar when bytes >= total or total == 0.
        let _ = self.event_tx.send(UploadEvent::CaptureEmbedProgress {
            task_id: task_id.to_string(),
            bytes: if res.is_ok() { total } else { 0 },
            total,
        });
        let tagged_in_place = match res {
            Ok(()) => {
                self.capture_embedded
                    .lock()
                    .expect("capture_embedded lock")
                    .insert(task_id.to_string());
                // Source bytes changed after the staging hash → invalidate so the
                // upload worker re-derives the digest from the tagged file.
                if let Err(e) = self.db.clear_upload_hashes(task_id).await {
                    tracing::warn!(task_id, "failed to clear hashes after in-place embed: {e}");
                }
                tracing::info!(task_id, "[capture] embedded metadata in place at save");
                true
            }
            Err(crate::capture::CaptureEmbedError::ExiftoolNotFound) => {
                // exiftool missing breaks the whole capture feature — surface it and
                // do NOT auto-advance (the clip's metadata is still recorded, but an
                // untagged upload is not what the user expects from a failed save).
                return Err(UploadError::CaptureEmbed {
                    message: "exiftool binary not found".to_string(),
                });
            }
            Err(e) => {
                tracing::warn!(
                    task_id,
                    "[capture] in-place embed failed at save ({e}); a copy will be tagged at upload"
                );
                false
            }
        };
        // Persist the resolution so it survives a restart even when the file was
        // NOT tagged (write-failed in-place embed → 'filled', a copy is tagged at
        // upload; success → 'embedded'). This is the durable backing that
        // `recover_capture_for_staged` rehydrates from. Best-effort.
        let status = if tagged_in_place {
            "embedded"
        } else {
            "filled"
        };
        if let Err(e) = self
            .db
            .set_capture_row(task_id, status, meta_json.as_deref())
            .await
        {
            tracing::warn!(task_id, "failed to persist capture row: {e}");
        }
        // Metadata is now RESOLVED (recorded in memory at the top of this fn, and
        // tagged into the file on the in-place path). Auto-advance the clip to
        // upload without a manual click — held only if it's transcode-eligible.
        self.auto_advance_if_resolved(task_id).await;
        Ok(tagged_in_place)
    }

    /// Recover capture state for `Staged` clips after a restart: the in-memory
    /// maps are lost, but the tags live in the files. For each staged clip with no
    /// in-memory entry, read the embedded tags back via a local ffprobe and, when
    /// present, repopulate `capture_metadata` + `capture_embedded` so the row shows
    /// "✓ filled" and the upload doesn't re-embed. Also picks up vendor-pre-tagged
    /// files. One file at a time (each is a quick moov-only probe). Returns whether
    /// any row was recovered (the caller bumps the UI's capture revision if so).
    pub async fn recover_capture_for_staged(self: &Arc<Self>) -> bool {
        let staged = match self.db.get_staged_uploads().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("recover_capture_for_staged: failed to load staged set: {e}");
                return false;
            }
        };
        let mut recovered = false;
        for task in staged {
            if self.capture_resolved(&task.id) {
                continue;
            }
            // Durable state first: a clip whose in-place embed FAILED has no tags
            // in the file, but its resolution was persisted to the row. Rehydrate
            // from there so it uploads with its metadata (a copy is tagged at
            // upload) instead of silently untagged.
            if let Ok(Some((status, json))) = self.db.get_capture_row(&task.id).await
                && self.hydrate_capture_from_row(&task.id, &status, json.as_deref())
            {
                recovered = true;
                tracing::info!(task_id = %task.id, status = %status, "[capture] rehydrated resolution from row");
                continue;
            }
            // Fallback: read tags embedded in the file (vendor-pre-tagged, or an
            // in-place embed whose in-memory maps we lost with no row record).
            let path = PathBuf::from(&task.local_path);
            let parsed =
                tokio::task::spawn_blocking(move || crate::capture::read_embedded_capture(&path))
                    .await
                    .ok()
                    .flatten();
            if let Some(meta) = parsed {
                self.set_capture_metadata(&task.id, Some(meta));
                self.capture_embedded
                    .lock()
                    .expect("capture_embedded lock")
                    .insert(task.id.clone());
                recovered = true;
                tracing::info!(task_id = %task.id, "[capture] recovered embedded metadata from file");
            }
        }
        recovered
    }

    /// Rehydrate the in-memory capture maps for `task_id` from a persisted row
    /// resolution (`status` + optional serialized `CaptureMetadata`). Returns
    /// whether anything was hydrated. `status` is a plain column string, so the
    /// unrecognized/`none` case falls through to `false`.
    fn hydrate_capture_from_row(&self, task_id: &str, status: &str, json: Option<&str>) -> bool {
        match status {
            "skipped" => {
                self.skip_capture_metadata(task_id);
                true
            }
            "filled" | "embedded" => {
                let Some(json) = json else {
                    return false;
                };
                let Ok(meta) = serde_json::from_str::<crate::capture::CaptureMetadata>(json) else {
                    return false;
                };
                self.set_capture_metadata(task_id, Some(meta));
                if status == "embedded" {
                    self.capture_embedded
                        .lock()
                        .expect("capture_embedded lock")
                        .insert(task_id.to_string());
                }
                true
            }
            _ => false,
        }
    }

    /// Batch save: embed `meta` into every clip currently `Staged`, ONE AT A TIME
    /// (each is a full-file rewrite — never run concurrently). Also records `meta`
    /// as the default for files added later (caller sets that). Per-file failures
    /// are logged and skipped (their values stay in memory for the upload-time
    /// copy fallback); the loop continues so one bad clip doesn't block the rest.
    pub async fn apply_capture_to_staged(self: &Arc<Self>, meta: crate::capture::CaptureMetadata) {
        let staged = match self.db.get_staged_uploads().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("apply_capture_to_staged: failed to load staged set: {e}");
                return;
            }
        };
        for task in staged {
            if let Err(e) = self.embed_capture_in_place(&task.id, meta.clone()).await {
                tracing::warn!(task_id = %task.id, "batch capture embed failed: {e}");
            }
        }
    }

    /// Return the cached `whoami` snapshot, fetching it on first call
    /// and reusing it for the engine's lifetime. `None` on whoami
    /// failure or when the response has no `user` (Firebase-only auth
    /// without a tenant join). Callers MUST treat `None` as "skip the
    /// same-user reuse branch and fall back to a generic cross-tenant
    /// message" rather than as an error — the rest of the dedup gate
    /// still works.
    async fn current_user_cache(&self) -> Option<&CurrentUserCache> {
        self.current_user_cache
            .get_or_try_init(|| async {
                let resp = self.api.whoami().await.map_err(|e| {
                    tracing::warn!("whoami failed during dedup check: {e}");
                })?;
                let Some(user) = resp.user else {
                    tracing::warn!("whoami returned no user — skipping reuse branch");
                    return Err(());
                };
                Ok(CurrentUserCache {
                    user_id: user.id,
                    tenants: user.tenant_infos.unwrap_or_default(),
                })
            })
            .await
            .ok()
    }

    /// Update the auto-clean flag at runtime. Takes effect on the next
    /// upload that finishes — already-completed tasks have already decided.
    pub fn set_auto_clean(&self, value: bool) {
        self.auto_clean.store(value, Ordering::Relaxed);
    }

    /// Read the current auto-clean flag.
    pub fn auto_clean(&self) -> bool {
        self.auto_clean.load(Ordering::Relaxed)
    }

    /// Run the server-side video quality check at staging time.
    ///
    /// Returns `(video_info, warnings, verdict)`. `video_info` mirrors
    /// the server's probe output for the per-row popover; `warnings`
    /// are advisory lines (recommend bands, telemetry hints);
    /// `verdict` defers `Stage` vs `Reject` to the hash worker.
    ///
    /// Errors:
    ///   * `VideoUnplayable` — no `moov` atom, surfaced before any
    ///     network call so power-cut recordings fail fast.
    ///   * `QualityCheckPayloadTooLarge` — assembled head-bytes exceed
    ///     the 16 MiB cap. Real-world camera output stays well under
    ///     this; hitting it means the input is malformed.
    ///   * `QualityCheckOffline` — server unreachable. Hard cutover
    ///     means we cannot fall back to a local rule check.
    async fn run_quality_check(
        &self,
        path: &Path,
        tenant_id: &str,
        project_id: &str,
        filename: &str,
    ) -> Result<
        (
            Option<crate::models::VideoInfo>,
            Vec<String>,
            PostHashVerdict,
        ),
        UploadError,
    > {
        // Magic-byte sniff before we touch the atom walker. The walker
        // assumes an ISO BMFF top-level layout; pointing it at a Matroska
        // or AVI file just produces noise and an opaque "no moov" error.
        // The 2026-05-16 production-data sweep showed 99.98% of customer
        // uploads are already ISO BMFF, so the right answer for the rest
        // is a kind-specific rejection before we spend any IO.
        let t_qc_entry = std::time::Instant::now();
        match container_kind::detect(path)? {
            ContainerKind::IsoBmff => {}
            kind @ (ContainerKind::Matroska
            | ContainerKind::WebM
            | ContainerKind::Avi
            | ContainerKind::Asf
            | ContainerKind::Flv
            | ContainerKind::MpegTs
            | ContainerKind::Unknown) => {
                return Err(UploadError::UnsupportedContainer { kind });
            }
        }
        let t_after_kind = t_qc_entry.elapsed();

        let path_buf = path.to_path_buf();
        let chunks =
            match tokio::task::spawn_blocking(move || video_head::extract_atom_chunks(&path_buf))
                .await
            {
                Ok(Ok(c)) => c,
                Ok(Err(VideoValidationError::Unplayable { reason })) => {
                    return Err(UploadError::VideoUnplayable { reason });
                }
                Ok(Err(VideoValidationError::MoovTooLarge { bytes, cap })) => {
                    return Err(UploadError::QualityCheckPayloadTooLarge { bytes, cap });
                }
                Ok(Err(VideoValidationError::Io(e))) => {
                    return Err(UploadError::Io(e));
                }
                Ok(Err(
                    e @ (VideoValidationError::FfprobeNotFound
                    | VideoValidationError::ProbeFailed(_)
                    | VideoValidationError::UnsupportedFormat(_)),
                )) => {
                    // The atom walker no longer produces these variants; if
                    // a future change reintroduces them we want to know
                    // rather than silently treating them as Unplayable.
                    tracing::warn!("Atom walker produced unexpected error for {filename}: {e}");
                    return Err(UploadError::VideoUnplayable {
                        reason: e.to_string(),
                    });
                }
                Err(join_err) => {
                    tracing::error!("Atom walker task panicked for {filename}: {join_err}");
                    return Err(UploadError::Io(std::io::Error::other(join_err)));
                }
            };

        let t_after_atomwalk = t_qc_entry.elapsed();
        let response = self
            .api
            .quality_check(tenant_id, project_id, chunks)
            .await?;
        let t_after_server = t_qc_entry.elapsed();
        tracing::debug!(
            filename = %filename,
            t_kind_ms = t_after_kind.as_millis() as u64,
            t_atomwalk_ms = (t_after_atomwalk - t_after_kind).as_millis() as u64,
            t_server_ms = (t_after_server - t_after_atomwalk).as_millis() as u64,
            t_total_ms = t_after_server.as_millis() as u64,
            "quality_check timings",
        );
        let verdict = match response.acceptance {
            Acceptance::Accepted => PostHashVerdict::Stage,
            Acceptance::Rejected { reasons } => PostHashVerdict::Reject(reasons),
        };
        Ok((Some(response.info), response.warnings, verdict))
    }

    /// Stage a file for review (step 1 of two-step upload).
    ///
    /// QC-only staging: this function returns synchronously after inserting
    /// the row (a video row in `QualityChecking`, a non-video row directly in
    /// `Staged`), and spawns a background worker that — for video — runs the
    /// local atom walk + server `/quality-check` round-trip and settles the
    /// row `Staged` (accept) or `Rejected` (QC failure). Staging does NOT
    /// hash, compute PDQ, or run any dedup check: the capture-metadata embed
    /// rewrites the file after staging, so a staging-time fingerprint can
    /// never match what is finally uploaded. Dedup is the sole job of the
    /// post-embed Stage-4 gate (`precreate_dedup`), where the digests match
    /// the stored object. The video QC phase emits `StateChanged` so the
    /// queue UI can render an indeterminate progress bar during the
    /// network-bound check.
    ///
    /// Failures along the quality-check phase (broken file, missing
    /// `moov`, unsupported container, server unreachable) settle the
    /// row directly into `Rejected` with a typed reason in
    /// `rejection_reasons`. They don't surface as a returned `Err`
    /// any more — keeping them on the row instead of in a transient
    /// toast is the whole point of the new state, so the user can
    /// see *which* file is broken and *why* without having to recall
    /// the toast.
    ///
    /// A super-admin "Force upload" on a `Rejected` row turns it into a
    /// normal pipeline run; the Stage-1 rehash derives the digest from the
    /// (post-embed) source then, so no staging-time hash is needed.
    pub async fn stage_file(
        self: &Arc<Self>,
        path: &Path,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<UploadTask, UploadError> {
        // Timing-instrumented to chase a 1–3 s freeze on file-pick.
        // Each numbered checkpoint logs the elapsed-ms-since-entry so
        // the slow segment is obvious in the rolling log without
        // needing a perf-sampler attached.
        let t_entry = std::time::Instant::now();
        if !path.exists() {
            return Err(UploadError::FileNotFound(path.to_path_buf()));
        }
        let t_after_exists = t_entry.elapsed();

        let metadata = std::fs::metadata(path)?;
        let t_after_metadata = t_entry.elapsed();
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mime_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        let is_video = mime_type.starts_with("video/");
        let t_after_mime = t_entry.elapsed();

        // Mirror the row's DEFAULT `datetime('now')` timestamps in the in-memory
        // task so the transfer panel shows an "Added" time immediately (the DB
        // stays the source of truth on reload). UTC, SQLite datetime shape.
        let now_ts = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let task = UploadTask {
            id: Uuid::new_v4().to_string(),
            local_path: path.to_string_lossy().to_string(),
            filename,
            size: metadata.len(),
            mime_type,
            tenant_id: tenant_id.to_string(),
            project_id: project_id.to_string(),
            document_id: None,
            session_id: None,
            mpu_upload_id: None,
            bytes_uploaded: 0,
            // Non-video rows skip the quality-check step entirely; the
            // server gate runs only for `video/*`. With staging-time dedup
            // removed (dedup now happens once, post-capture-embed, at
            // Stage 4), a non-video row has nothing to do at staging and
            // lands in `Staged` straight away. Video rows enter
            // `QualityChecking` and transition to `Staged` once the server
            // QC response arrives — they no longer pass through `Hashing`.
            state: if is_video {
                UploadState::QualityChecking
            } else {
                UploadState::Staged
            },
            error_message: None,
            hash: None,
            source_md5: None,
            source_crc32c: None,
            source_sha256_head_256kib: None,
            validation_warnings: Vec::new(),
            rejection_reasons: Vec::new(),
            retry_count: 0,
            transcode: false,
            transcoded_size: None,
            video_info: None,
            force_upload: false,
            created_at: now_ts.clone(),
            updated_at: now_ts,
        };

        self.db.insert_upload_task(&task).await?;
        // Apply the current batch-default capture metadata to this freshly-staged
        // task so it shows "✓ filled" immediately and is embedded at process time
        // without needing a per-file fill.
        if let Some(batch) = self
            .batch_capture
            .lock()
            .expect("batch_capture lock")
            .clone()
            && !batch.is_empty()
        {
            self.capture_metadata
                .lock()
                .expect("capture_metadata lock")
                .insert(task.id.clone(), batch);
        }
        let t_after_insert = t_entry.elapsed();
        let _ = self
            .event_tx
            .send(UploadEvent::TaskAdded(Box::new(task.clone())));
        let t_after_event = t_entry.elapsed();

        let engine = Arc::clone(self);
        let stage_sem = Arc::clone(&self.stage_semaphore);
        let task_id = task.id.clone();
        let path_buf = path.to_path_buf();
        let tenant_id_owned = tenant_id.to_string();
        let project_id_owned = project_id.to_string();
        let filename = task.filename.clone();
        tokio::spawn(async move {
            // Throttle the QC + hash churn to `MAX_CONCURRENT_STAGING`. The row
            // and its `TaskAdded` event are already out (above, unbounded), so
            // the user sees every file immediately; only the server round-trips
            // and hash streams queue behind this permit. Held for the whole
            // phase below and released on drop when the worker returns.
            let _permit = stage_sem.acquire().await.expect("stage semaphore closed");
            let final_state = if is_video {
                engine
                    .run_quality_check_only(
                        &task_id,
                        &path_buf,
                        &tenant_id_owned,
                        &project_id_owned,
                        &filename,
                    )
                    .await
            } else {
                // Non-video rows skip the quality-check probe entirely. Staging no
                // longer hashes or dedups (that is the sole job of Stage 4, after
                // the capture-metadata embed), so the row is already `Staged` from
                // insert: just settle it and let auto-advance decide whether to
                // upload. The duplicate verdict, if any, is caught at Stage 4 on
                // the post-embed bytes — the only fingerprints that match what is
                // actually stored.
                engine.settle_staged_and_advance(&task_id, Vec::new()).await
            };
            tracing::debug!(task_id = %task_id, ?final_state, "stage_file worker finished");
        });
        let t_after_spawn = t_entry.elapsed();

        // Single line per file so the log isn't spammed; the segment
        // breakdown is right here next to the row id. If any of these
        // jumps past ~50 ms on a small local file, that's the freeze.
        // Kept at debug — useful when chasing a regression, noise
        // otherwise, since a healthy run reads `t_total_ms=1`.
        tracing::debug!(
            task_id = %task.id,
            filename = %task.filename,
            t_exists_ms = t_after_exists.as_millis() as u64,
            t_metadata_ms = (t_after_metadata - t_after_exists).as_millis() as u64,
            t_mime_ms = (t_after_mime - t_after_metadata).as_millis() as u64,
            t_insert_ms = (t_after_insert - t_after_mime).as_millis() as u64,
            t_event_ms = (t_after_event - t_after_insert).as_millis() as u64,
            t_spawn_ms = (t_after_spawn - t_after_event).as_millis() as u64,
            t_total_ms = t_after_spawn.as_millis() as u64,
            "stage_file timings",
        );

        Ok(task)
    }

    /// Drive the quality-check phase and settle the row. Staging is now
    /// QC-only: there is NO hashing, NO PDQ compute, and NO dedup check at
    /// staging time. The capture-metadata embed (Stage 0) rewrites the file
    /// after staging, so any fingerprint taken here would be on the
    /// pre-embed bytes and could never match what is finally uploaded. Dedup
    /// is therefore deferred to the single post-embed gate at Stage 4
    /// (`precreate_dedup`), where the digests match the stored object.
    ///
    /// On a broken-file / unsupported-container / offline failure the row
    /// settles directly into `Rejected` with the typed message in
    /// `rejection_reasons`. On accept the row settles `Staged` and
    /// auto-advances if its capture metadata is already resolved. On a
    /// server QC *reject* the row settles `Rejected` with the QC reasons —
    /// never a *duplicate* rejection at staging.
    async fn run_quality_check_only(
        self: &Arc<Self>,
        task_id: &str,
        path: &Path,
        tenant_id: &str,
        project_id: &str,
        filename: &str,
    ) -> UploadState {
        let (video_info, warnings, post_hash) = match self
            .run_quality_check(path, tenant_id, project_id, filename)
            .await
        {
            Ok(triple) => triple,
            Err(err) => return self.settle_quality_check_rejected(task_id, &err).await,
        };
        // Read back any io.visionlab tags already embedded in the file (a
        // re-add of a Save-time-tagged clip, or a vendor-pre-tagged file):
        // pre-populate so the row shows "✓ filled" on add and the upload
        // skips re-embedding.
        if let Some(info) = video_info.as_ref()
            && let Some(parsed) = crate::capture::parse_capture_from_tags(&info.metadata)
        {
            let json = serde_json::to_string(&parsed).ok();
            self.set_capture_metadata(task_id, Some(parsed));
            self.capture_embedded
                .lock()
                .expect("capture_embedded lock")
                .insert(task_id.to_string());
            // The tags are in the file → persist as 'embedded' so a restart
            // rehydrates without a re-probe.
            if let Err(e) = self
                .db
                .set_capture_row(task_id, "embedded", json.as_deref())
                .await
            {
                tracing::warn!(task_id, "failed to persist capture row: {e}");
            }
        }
        // Persist the response payload (video_info + warnings) and the
        // terminal staging state in one write so the popover-data fields land
        // atomically with the state change. A QC reject lands `Rejected` with
        // the server reasons; an accept lands `Staged`.
        let (state, rejection_reasons) = match post_hash {
            PostHashVerdict::Stage => (UploadState::Staged, Vec::new()),
            PostHashVerdict::Reject(reasons) => (UploadState::Rejected, reasons),
        };
        let _ = self
            .db
            .update_upload_quality_check_settled(
                task_id,
                state.clone(),
                video_info.as_ref(),
                &warnings,
            )
            .await;
        let _ = self.event_tx.send(UploadEvent::QualityCheckPassed {
            task_id: task_id.to_string(),
            video_info,
            warnings: warnings.clone(),
        });
        if state == UploadState::Rejected {
            // Surface the QC rejection reasons + flip the row to Rejected.
            return self
                .settle_post_hash(task_id, state, warnings, rejection_reasons)
                .await;
        }
        // Accepted: emit warnings (may be empty) + the `Staged` transition,
        // then auto-advance if the clip's capture metadata is already
        // resolved (batch default in effect, or tags read back above).
        let _ = self.event_tx.send(UploadEvent::ValidationWarnings {
            task_id: task_id.to_string(),
            warnings,
            rejection_reasons,
        });
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task_id.to_string(),
            state: UploadState::Staged,
        });
        self.auto_advance_if_resolved(task_id).await;
        UploadState::Staged
    }

    /// Settle a (non-video) row `Staged` and auto-advance if its capture
    /// metadata is already resolved. The QC-only-staging counterpart to the
    /// accept branch of [`Self::run_quality_check_only`], for rows that never
    /// run a quality check. No hashing, no dedup: that is Stage 4's job now.
    async fn settle_staged_and_advance(
        self: &Arc<Self>,
        task_id: &str,
        warnings: Vec<String>,
    ) -> UploadState {
        let settled = self
            .settle_post_hash(task_id, UploadState::Staged, warnings, Vec::new())
            .await;
        if settled == UploadState::Staged {
            self.auto_advance_if_resolved(task_id).await;
        }
        settled
    }

    /// Settle a quality-check failure as a typed `Rejected` row. The
    /// error's `Display` becomes the single rejection reason; the
    /// `is_expected()` classifier still controls whether the failure
    /// is a `warn!` (broken file, offline) or an `error!` (bug we
    /// should hear about), matching how `Self::log` would route it.
    async fn settle_quality_check_rejected(
        self: &Arc<Self>,
        task_id: &str,
        err: &UploadError,
    ) -> UploadState {
        err.log(format_args!("Quality check for {task_id}"));
        let reason = err.to_string();
        self.settle_post_hash(task_id, UploadState::Rejected, Vec::new(), vec![reason])
            .await
    }

    /// Auto-advance one clip from `Staged` to upload when, and only when, it is
    /// ready: it is still `Staged`, its capture metadata is RESOLVED (filled or
    /// skipped), and it is NOT held for the opt-in transcode flow. The single
    /// gate for the skip-or-fill → auto-upload path. Idempotent and safe to call
    /// speculatively (e.g. from a fill/skip handler that may target a clip that
    /// has already advanced): a non-`Staged` row, an unresolved row, or a
    /// transcode-held row is left untouched. Re-reads the row from the DB so the
    /// transcode hold can consult the persisted `video_info`.
    pub async fn auto_advance_if_resolved(self: &Arc<Self>, task_id: &str) {
        if !self.capture_resolved(task_id) {
            return;
        }
        let task = match self.db.get_upload_by_id(task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(task_id, "auto-advance: failed to load row: {e}");
                return;
            }
        };
        if task.state != UploadState::Staged {
            return;
        }
        if self.held_for_transcode(task.video_info.as_deref()) {
            return;
        }
        self.advance_staged_and_dispatch(task_id).await;
    }

    /// Whether auto-advance should HOLD this clip `Staged` for the manual
    /// opt-in transcode flow. The hold fires iff the transcode feature is on
    /// AND transcoding would actually shrink this specific clip — mirroring
    /// the UI toggle gate and the `maybe_transcode` short-circuit exactly.
    /// Re-reads (never modifies) `video::transcode_would_help`. `None`
    /// `video_info` (non-video, or a probe that returned nothing) is never
    /// held.
    fn held_for_transcode(&self, video_info: Option<&crate::models::VideoInfo>) -> bool {
        let Some(info) = video_info else {
            return false;
        };
        self.transcode_config.enabled && video::transcode_would_help(info, &self.transcode_config)
    }

    /// Flip a freshly-`Staged` row to `Pending` (DB + UI event) and dispatch
    /// it through the bounded-parallel worker. Mirrors what one iteration of
    /// the `confirm_staged` loop does for a single task, so auto-advance and
    /// the manual confirm share one dispatch path. Flipping to `Pending`
    /// before the load means the manual `[Upload]` button (which reads
    /// STAGED rows) can never also grab this row.
    async fn advance_staged_and_dispatch(self: &Arc<Self>, task_id: &str) {
        if let Err(e) = self
            .db
            .update_upload_state(task_id, UploadState::Pending, None)
            .await
        {
            tracing::warn!(task_id, "auto-advance: failed to flip row to Pending: {e}");
            return;
        }
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task_id.to_string(),
            state: UploadState::Pending,
        });
        let pending = match self.db.get_pending_uploads().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(task_id, "auto-advance: failed to load PENDING set: {e}");
                return;
            }
        };
        let Some(task) = pending.into_iter().find(|t| t.id == task_id) else {
            tracing::warn!(
                task_id,
                "auto-advance: row not found in PENDING set after flip"
            );
            return;
        };
        self.dispatch_one(task);
    }

    /// Spawn the bounded-parallel upload worker for one already-`Pending`
    /// task. The single per-task dispatch path: a permit from the
    /// `upload_semaphore` caps concurrency at `max_concurrent`, and a failure
    /// settles the row to `Failed` with the typed error. Driven by
    /// `confirm_staged` (the manual "Upload") and `force_upload`. Per-task (not
    /// per-batch) so a slow QC file never gates a fast one.
    ///
    /// `pub` so the UI's manual Retry / Resume enter through this bounded,
    /// single-flight path too, instead of spawning `process_task` directly —
    /// a bare spawn bypasses `upload_semaphore` and `try_enter_flight`, letting
    /// more than `max_concurrent` files upload at once (and re-driving a row
    /// already in flight). This is the ONLY sanctioned way to start a worker.
    pub fn dispatch_one(self: &Arc<Self>, mut task: UploadTask) {
        let engine = Arc::clone(self);
        let sem = Arc::clone(&self.upload_semaphore);
        tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            // Single-flight: if another worker in this process is already driving
            // this id, drop this dispatch (closes the auto_advance-vs-auto_advance,
            // force-on-in-flight, and resume-vs-retry double-run classes).
            let Some(_flight) = engine.try_enter_flight(&task.id) else {
                return;
            };
            match engine.process_task(&mut task).await {
                Ok(()) => tracing::info!("Upload completed: {}", task.filename),
                // User pause: settle to Paused, not Failed — a hold, not a failure.
                Err(UploadError::Cancelled) => {
                    engine.settle_cancelled_as_paused(&task.id).await;
                }
                Err(e) => {
                    e.log(format_args!("Upload of {}", task.filename));
                    // Guarded: never clobber a COMPLETED a sibling just recorded.
                    let _ = engine
                        .db
                        .settle_failure(&task.id, UploadState::Failed, &e.to_string())
                        .await;
                    let _ = engine.event_tx.send(UploadEvent::Failed {
                        task_id: task.id,
                        error: e.to_string(),
                    });
                }
            }
        });
    }

    /// One write + two events: persist state + warnings + reasons,
    /// notify the UI, return the state for logging. Shared by both
    /// terminal paths so the DB and event order can't drift between
    /// them.
    async fn settle_post_hash(
        &self,
        task_id: &str,
        state: UploadState,
        warnings: Vec<String>,
        rejection_reasons: Vec<String>,
    ) -> UploadState {
        let _ = self
            .db
            .update_upload_state_warnings_and_reasons(
                task_id,
                state.clone(),
                &warnings,
                &rejection_reasons,
            )
            .await;
        let _ = self.event_tx.send(UploadEvent::ValidationWarnings {
            task_id: task_id.to_string(),
            warnings,
            rejection_reasons,
        });
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task_id.to_string(),
            state: state.clone(),
        });
        state
    }

    /// Pump the hash stream into UI events + persisted hashes. Since
    /// staging no longer hashes (dedup moved to the post-embed Stage-4
    /// gate), the sole caller is `process_task`'s Stage-1 rehash, which
    /// `?`-propagates an error. Errors are surfaced as `UploadError::Io`
    /// so that caller can `?` them. `HashProgress` events fire
    /// unconditionally — a row that isn't in a hashing-progress view has
    /// its stale entry cleared by the next `StateChanged` in the UI's
    /// event handler.
    async fn consume_hash_stream(
        &self,
        task_id: &str,
        path: &Path,
    ) -> Result<dedup::FileHashes, UploadError> {
        use tokio_stream::StreamExt;

        let mut stream = Box::pin(dedup::hash_file_full_stream(path));
        while let Some(event) = stream.next().await {
            match event {
                dedup::HashEvent::Progress {
                    bytes_so_far,
                    total_bytes,
                } => {
                    let _ = self.event_tx.send(UploadEvent::HashProgress {
                        task_id: task_id.to_string(),
                        bytes_hashed: bytes_so_far,
                        total_bytes,
                    });
                }
                dedup::HashEvent::Done(hashes) => {
                    let _ = self.db.update_upload_hashes(task_id, &hashes).await;
                    return Ok(hashes);
                }
                dedup::HashEvent::Error(e) => {
                    tracing::warn!(task_id, "hash stream error: {e}");
                    return Err(UploadError::Io(std::io::Error::other(e)));
                }
            }
        }
        Err(UploadError::Io(std::io::Error::other(
            "hash stream ended without Done",
        )))
    }

    /// Ask the cross-tenant dedup registry whether this file's digest is
    /// already known and decide what staging should do with the row.
    ///
    /// The three outcomes are spelled out on [`DedupVerdict`]. A network
    /// or API failure on the dedup check itself swallows the error and
    /// returns `Allow` so a transient outage doesn't block staging — the
    /// worst case is the user uploads a duplicate, which is the
    /// pre-feature behaviour.
    ///
    /// The `Reuse` branch costs one extra `GET /documents/{id}` per
    /// same-user candidate match (to read `gcs_uri`); we stop at the
    /// first reusable hit. Other-user matches and other-tenant counts
    /// fall through to the existing `Reject` paths unchanged.
    ///
    /// Uses the multi-signal V2 `/digest-checks` endpoint: we send the
    /// full `{md5, crc32c, sha256_head_256kib}` digest so the server can
    /// match on the verified `(crc32c, sha256_head_256kib)` pair, not
    /// just md5. This catches files first uploaded via a resumable path
    /// — where GCS surfaces a crc32c but never an md5 — that the legacy
    /// md5-only gate (`/dedup-checks`) missed entirely.
    async fn dedup_verdict(
        &self,
        tenant_id: &str,
        hashes: &dedup::FileHashes,
        pdq_frames: Vec<PdqFrameWire>,
        filename: &str,
    ) -> DedupVerdict {
        let candidate = DigestCheckCandidate {
            digest: Digest {
                md5: Some(hashes.md5_hex.clone()),
                crc32c: Some(hashes.crc32c_b64.clone()),
                sha256_head_256kib: Some(hashes.sha256_head_256kib_hex.clone()),
            },
            // Sent inline so the server runs the same-tenant PDQ coverage scan
            // without its sidecar. Empty (PDQ disabled / nothing decoded) → omit
            // the field, leaving an exact-only query.
            pdq_frames: (!pdq_frames.is_empty()).then_some(pdq_frames),
        };
        let resp = match self.api.check_digests(tenant_id, &candidate).await {
            Ok(resp) => resp,
            Err(e) => {
                tracing::warn!("Dedup check failed for {filename}: {e}");
                return DedupVerdict::Allow;
            }
        };
        // One candidate in → at most one result row out, so no md5
        // correlation is needed: take the single row if present.
        let Some(result) = resp.results.into_iter().next() else {
            tracing::debug!(
                tenant_id,
                filename,
                "dedup: empty digest-check response — treating as not-a-duplicate"
            );
            return DedupVerdict::Allow;
        };
        let tenant_match_count = result.tenant_matches.len();
        let user_other_tenant_count = result.user_other_tenant_ids.len();
        tracing::debug!(
            tenant_id,
            filename,
            tenant_match_count,
            user_other_tenant_count,
            near_duplicate = result.near_duplicate.is_some(),
            "dedup: registry response"
        );
        if tenant_match_count == 0 && user_other_tenant_count == 0 {
            // Exact gate missed. If the server escalated to PDQ and found a
            // same-tenant perceptual near-duplicate, reject on that; otherwise
            // the file is genuinely new.
            return match result.near_duplicate {
                Some(near) => {
                    tracing::debug!(
                        tenant_id,
                        filename,
                        document_id = %near.document_id,
                        coverage = near.coverage,
                        matched_frames = near.matched_frames,
                        "dedup: PDQ near-duplicate hit"
                    );
                    DedupVerdict::Reject(near_duplicate_message(&near))
                }
                None => DedupVerdict::Allow,
            };
        }
        if tenant_match_count == 0 {
            return DedupVerdict::Reject(
                self.cross_tenant_message(&result.user_other_tenant_ids)
                    .await,
            );
        }
        if let Some(reuse_id) = self
            .find_reusable_tenant_match(tenant_id, &result.tenant_matches, filename)
            .await
        {
            return DedupVerdict::Reuse(reuse_id);
        }
        DedupVerdict::Reject(
            self.tenant_match_message(tenant_id, &result.tenant_matches)
                .await,
        )
    }

    /// Build a friendly cross-tenant rejection message that names the
    /// tenants where the user uploaded this file. Each ID is joined
    /// against the cached tenant list to render the user-facing
    /// `display_name`; an unknown ID (rare — user lost membership
    /// since `whoami`, or the cache failed to load) falls back to the
    /// raw ID. If the cache itself is unavailable, falls back to the
    /// generic pre-feature wording.
    async fn cross_tenant_message(&self, other_tenant_ids: &[String]) -> String {
        let Some(cache) = self.current_user_cache().await else {
            return "You uploaded this file in another tenant".to_string();
        };
        let names: Vec<&str> = other_tenant_ids
            .iter()
            .map(|id| {
                cache
                    .tenants
                    .iter()
                    .find(|t| t.id == *id)
                    .map(|t| t.display_name.as_str())
                    .unwrap_or(id.as_str())
            })
            .collect();
        match names.as_slice() {
            [] => "You uploaded this file in another tenant".to_string(),
            [one] => format!("You uploaded this file in tenant '{one}'"),
            _ => format!("You uploaded this file in tenants: {}", names.join(", ")),
        }
    }

    /// Friendly same-tenant "already uploaded" message that names the
    /// organization and the project(s) the file already lives in. The
    /// `/digest-checks` response carries `projectId`/`tenantId` per match;
    /// this resolves them to display names — the org from the cached whoami
    /// tenant list, each project via a `list_projects` lookup. Any resolution
    /// failure degrades gracefully (org → "this organization", project → its
    /// id): a cosmetic message must never change or fail the dedup verdict.
    async fn tenant_match_message(
        &self,
        tenant_id: &str,
        matches: &[crate::models::DedupCheckMatch],
    ) -> String {
        let count = matches.len();
        let plural = if count == 1 { "" } else { "s" };

        // Org display name (the calling tenant), best-effort via the whoami cache.
        let org = self
            .current_user_cache()
            .await
            .and_then(|c| {
                c.tenants
                    .iter()
                    .find(|t| t.id.as_str() == tenant_id)
                    .map(|t| t.display_name.clone())
            })
            .unwrap_or_else(|| "this organization".to_string());

        // Distinct project ids from the matches, resolved to names. One
        // `list_projects` call on this rare reject path is acceptable.
        let mut project_ids: Vec<&str> = matches.iter().map(|m| m.project_id.as_str()).collect();
        project_ids.sort_unstable();
        project_ids.dedup();
        let projects: Vec<String> = match self.api.list_projects(tenant_id).await {
            Ok(list) => project_ids
                .iter()
                .map(|pid| {
                    list.iter()
                        .find(|p| p.id.as_str() == *pid)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| (*pid).to_string())
                })
                .collect(),
            Err(e) => {
                tracing::warn!(
                    "dedup: list_projects failed building the duplicate message; using project ids: {e}"
                );
                project_ids.iter().map(|p| (*p).to_string()).collect()
            }
        };

        match projects.as_slice() {
            [] => format!("Already uploaded in '{org}' ({count} document{plural})"),
            [one] => {
                format!("Already uploaded in '{org}', project '{one}' ({count} document{plural})")
            }
            many => format!(
                "Already uploaded in '{org}', projects: {} ({count} document{plural})",
                many.join(", ")
            ),
        }
    }

    /// Pick the first tenant match whose `creator_id` is the logged-in
    /// user AND whose document still has no `gcs_uri` — i.e. the row
    /// the user themselves started uploading earlier and never
    /// finished. The check is per-candidate so we stop on the first
    /// reusable hit. `None` means "no such recoverable match"; the
    /// caller falls back to the regular Reject path.
    async fn find_reusable_tenant_match(
        &self,
        tenant_id: &str,
        matches: &[crate::models::DedupCheckMatch],
        filename: &str,
    ) -> Option<String> {
        let user_id = self.current_user_cache().await?.user_id.as_str();
        for m in matches.iter().filter(|m| m.creator_id == user_id) {
            match self
                .api
                .get_document(tenant_id, &m.project_id, &m.document_id)
                .await
            {
                Ok(doc) if doc.gcs_uri.is_none() => {
                    tracing::info!(
                        document_id = %m.document_id,
                        project_id = %m.project_id,
                        filename,
                        "dedup: reusing abandoned same-user upload (gcs_uri is null)"
                    );
                    return Some(m.document_id.clone());
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        document_id = %m.document_id,
                        "dedup: get_document failed, skipping reuse candidate: {e}"
                    );
                }
            }
        }
        None
    }

    /// The mandatory dedup gate run immediately before every `create_document`
    /// (see the invariant at the Stage-4 call site). Two layers:
    ///
    ///   1. **Local** — a concurrent sibling task for this exact content
    ///      (possibly in a second app instance sharing this SQLite DB) is
    ///      already uploading. Only the deterministic winner (MIN created_at,id)
    ///      creates; the rest defer (`AlreadyInFlight`). The server cannot see a
    ///      sibling that has not created its document yet, so this catches the
    ///      both-pre-create race the server check misses.
    ///   2. **Server** — `dedup_verdict` (`POST /digest-checks`) finds an
    ///      existing document for this content: the in-flight orphan a lost
    ///      create-response left behind (→ `Adopt`, since it is gcs_uri-null and
    ///      same-user) or a completed/cross-tenant/perceptual duplicate
    ///      (→ `Rejected`).
    ///
    /// `force_upload` bypasses the gate (the super-admin asked for a copy). A
    /// row missing any server digest leg can't be checked server-side, so it
    /// falls through to `Create` (the local layer still applies).
    async fn precreate_dedup(
        &self,
        task: &UploadTask,
        hash: &str,
        pdq_frames: Vec<PdqFrameWire>,
    ) -> PreCreate {
        if task.force_upload {
            return PreCreate::Create;
        }
        if let Ok(Some((winner_id, winner_doc))) = self.db.find_inflight_sibling_winner(hash).await
            && winner_id != task.id
        {
            return PreCreate::AlreadyInFlight {
                existing_id: winner_doc.unwrap_or(winner_id),
            };
        }
        let (Some(md5_hex), Some(crc32c_b64), Some(sha256_head_256kib_hex)) = (
            task.source_md5.clone(),
            task.source_crc32c.clone(),
            task.source_sha256_head_256kib.clone(),
        ) else {
            return PreCreate::Create;
        };
        let hashes = dedup::FileHashes {
            blake3_hex: hash.to_string(),
            md5_hex,
            crc32c_b64,
            sha256_head_256kib_hex,
        };
        match self
            .dedup_verdict(&task.tenant_id, &hashes, pdq_frames, &task.filename)
            .await
        {
            DedupVerdict::Allow => PreCreate::Create,
            DedupVerdict::Reuse(existing_id) => PreCreate::Adopt(existing_id),
            DedupVerdict::Reject(reason) => PreCreate::Rejected(reason),
        }
    }

    /// Super-admin override: flip `force_upload` on a `Rejected` task,
    /// reset it to `Pending`, and spawn the upload worker. The worker
    /// reads `task.force_upload` in Stage 1 and skips the local-DB
    /// dedup short-circuit, so a re-staged duplicate row proceeds
    /// instead of bouncing again. Quality-rejected rows take the same
    /// path: `process_task` doesn't re-run the acceptance gate, so the
    /// only effect of `Rejected → Pending` is that the pipeline runs.
    pub async fn force_upload(self: &Arc<Self>, task_id: &str) -> Result<(), UploadError> {
        self.db.force_upload_task(task_id).await?;
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task_id.to_string(),
            state: UploadState::Pending,
        });

        let pending = self.db.get_pending_uploads().await?;
        let Some(task) = pending.into_iter().find(|t| t.id == task_id) else {
            tracing::warn!("force_upload: task {task_id} not found in PENDING set");
            return Ok(());
        };
        self.dispatch_one(task);
        Ok(())
    }

    /// Confirm staged files for upload — the manual "Upload" step, kept for the
    /// transcode opt-in and as the explicit catch-up for any clip the auto-advance
    /// path left `Staged`. Moves each STAGED task whose required capture metadata is
    /// RESOLVED (filled OR skipped) to PENDING and dispatches it through the
    /// bounded-parallel worker; clips still unresolved are left `Staged` (showing
    /// the "Needs metadata" prompt) so the user can fill or skip them and click
    /// again. With auto-advance, a resolved non-transcode clip normally uploads on
    /// its own; this button still drives the transcode-held clips, which auto-
    /// advance deliberately holds for the opt-in confirm.
    pub async fn confirm_staged(
        self: &Arc<Self>,
        transcode_task_ids: &[String],
    ) -> Result<Vec<String>, DbError> {
        let staged = self.db.get_staged_uploads().await?;
        let mut confirmed_ids = Vec::new();

        for mut task in staged {
            // Capture must be RESOLVED (filled or skipped): a clip the user has
            // neither filled nor skipped stays `Staged` (its row shows "Needs
            // metadata") rather than being dispatched — they resolve it and click
            // Upload again.
            if !self.capture_resolved(&task.id) {
                continue;
            }
            task.transcode = transcode_task_ids.contains(&task.id);
            // Persist the transcode choice so resume-after-crash keeps it.
            // Without this, a killed mid-transcode task silently falls through
            // to uploading the original file on next launch.
            self.db
                .update_upload_transcode(&task.id, task.transcode)
                .await?;
            self.db
                .update_upload_state(&task.id, UploadState::Pending, None)
                .await?;
            task.state = UploadState::Pending;
            confirmed_ids.push(task.id.clone());

            self.dispatch_one(task);
        }

        Ok(confirmed_ids)
    }

    /// Remove a staged file (before upload confirmation).
    pub async fn remove_staged(&self, task_id: &str) -> Result<(), DbError> {
        self.db.delete_upload_task(task_id).await?;
        Ok(())
    }

    /// Remove a queued/failed upload row, first aborting any in-flight
    /// server-side upload so a partially-uploaded file does not leak a GCS
    /// resumable session or an incomplete XML multipart upload (both linger and
    /// cost storage until a bucket lifecycle rule reaps them). Use this for the
    /// transfer-panel "Remove"/"Clear" affordance, where a row may already have
    /// pushed bytes to GCS.
    ///
    /// The abort is best-effort: an unreachable server is logged and does NOT
    /// block the local-row deletion (the user asked to remove it). A `Completed`
    /// row has nothing in flight, so it is deleted directly. The created
    /// document is intentionally left in place — reaping a created-but-unverified
    /// document is a backend-owned decision handled separately.
    pub async fn abort_in_flight_and_remove(&self, task_id: &str) -> Result<(), DbError> {
        if let Some(task) = self.db.get_upload_by_id(task_id).await?
            && task.state != UploadState::Completed
        {
            self.abort_in_flight(&task).await;
        }
        self.db.delete_upload_task(task_id).await
    }

    /// Best-effort cancel of a task's in-flight server-side upload. Prefers the
    /// XML MPU path (its `uploadId` plus the document are required to abort) and
    /// falls back to the GCS resumable session. A no-op when neither is present
    /// (the task never reached the upload stage). Errors are logged, never
    /// returned: the caller is removing the row regardless.
    async fn abort_in_flight(&self, task: &UploadTask) {
        if let (Some(upload_id), Some(doc_id)) =
            (task.mpu_upload_id.as_deref(), task.document_id.as_deref())
        {
            if let Err(e) = self
                .api
                .abort_multipart_upload(&task.tenant_id, &task.project_id, doc_id, upload_id)
                .await
            {
                tracing::warn!(
                    "Abort multipart upload on remove failed for {}: {e}",
                    task.filename
                );
            }
            return;
        }
        if let Some(session_id) = task.session_id.as_deref() {
            let session = storage::UploadSession {
                session_id: session_id.to_string(),
                total_size: task.size,
                bytes_confirmed: task.bytes_uploaded,
            };
            if let Err(e) = self.storage.abort_upload(&session).await {
                tracing::warn!(
                    "Abort resumable session on remove failed for {}: {e}",
                    task.filename
                );
            }
        }
    }

    /// Process a single upload task through all stages.
    /// Resumes from where it left off — skips stages already completed
    /// (has document_id → skip create, has session_id → skip initiate).
    pub(crate) async fn process_task(&self, task: &mut UploadTask) -> Result<(), UploadError> {
        let original_buf = std::path::PathBuf::from(&task.local_path);

        // Temp copies (capture-tagged / transcoded / desensitized) are cleaned up
        // on EVERY exit of this function by this guard. Declared before Stage 0 so
        // the capture-tagged copy is tracked too.
        let mut temp_guard = TempCleanupGuard::default();

        // Stage 0: Ensure io.visionlab capture metadata is embedded in the upload
        // source. The normal path is Save-time `embed_capture_in_place` (tagged the
        // user's own file already → `capture_embedded` holds this task → nothing to
        // do here; the source is self-describing). This block is the FALLBACK for
        // clips whose Save-time in-place write failed (read-only / full removable
        // media): tag a local COPY now and upload that, so the GCS object still
        // carries the tags for server-side backfill. Only the fallback COPY is
        // tracked for temp cleanup — never the user's own file. The rewrite changes
        // the bytes, so Stage 1 re-derives the digest from the tagged file.
        let already_embedded = self
            .capture_embedded
            .lock()
            .expect("capture_embedded lock")
            .contains(&task.id);
        let capture_meta = if already_embedded {
            None
        } else {
            self.capture_metadata
                .lock()
                .expect("capture_metadata lock")
                .get(&task.id)
                .cloned()
        };
        let source_buf = match capture_meta {
            Some(meta) if !meta.is_empty() => {
                let input = original_buf.clone();
                let scratch = std::env::temp_dir().join("linewise-capture");
                let outcome = tokio::task::spawn_blocking(move || {
                    crate::capture::embed_capture_metadata_blocking(&input, &meta, &scratch)
                })
                .await
                .expect("capture embed task panicked")
                .map_err(|e| UploadError::CaptureEmbed {
                    message: e.to_string(),
                })?;
                if outcome.is_temp_copy {
                    temp_guard.track(Some(&outcome.path));
                }
                // Force Stage 1 to rehash the now-tagged file.
                task.hash = None;
                task.source_md5 = None;
                task.source_crc32c = None;
                task.source_sha256_head_256kib = None;
                tracing::info!(
                    "[capture] embedded metadata for {} ({})",
                    task.filename,
                    if outcome.is_temp_copy {
                        "tagged local copy"
                    } else {
                        "in place"
                    }
                );
                outcome.path
            }
            _ => original_buf.clone(),
        };
        let path = source_buf.as_path();

        // Stage 1: Hash the (post-capture-embed) upload source. Staging is
        // now QC-only — it does NOT hash — so a freshly-staged row reaches
        // here with no digest and `needs_rehash` is true. The hash is taken
        // on `path`, which is the capture-tagged artifact when Stage 0
        // embedded metadata: the digest therefore matches the bytes we are
        // about to upload, which is the whole point of moving dedup here.
        // `needs_rehash` also fires for:
        //
        //   1. Resumed legacy rows staged before any digest pass landed
        //      (`task.hash` is None).
        //   2. Resumed rows from the BLAKE3+MD5-only era: `task.hash` is set
        //      but `source_crc32c` / `source_sha256_head_256kib` are NULL.
        //      Without rehashing, Stage 4's `create_document` would send a
        //      partial `digest` and the GCS-callback verified pair couldn't
        //      match the desktop-supplied legs.
        //
        // A fully-hashed resumed row (all three legs present, e.g. a task
        // that already passed Stage 4 once and is resuming the upload) skips
        // the rehash and reuses its persisted digest.
        //
        // The `force_upload` flag suppresses the local-DB short-circuit — a
        // force-upload row reaches `process_task` only because a super-admin
        // clicked the bypass, so re-asserting the gate here would defeat it.
        let needs_rehash = task.hash.is_none()
            || task.source_crc32c.is_none()
            || task.source_sha256_head_256kib.is_none();
        if needs_rehash {
            let hashes = self.consume_hash_stream(&task.id, path).await?;
            task.hash = Some(hashes.blake3_hex.clone());
            task.source_md5 = Some(hashes.md5_hex.clone());
            task.source_crc32c = Some(hashes.crc32c_b64.clone());
            task.source_sha256_head_256kib = Some(hashes.sha256_head_256kib_hex.clone());

            // Local in-session double-add guard: this exact content (by
            // post-embed BLAKE3) already finished uploading from this machine
            // (`file_hashes` is written only after Verify, Stage 6). This used
            // to run at staging on the pre-embed bytes; moving it here keeps
            // the protection but now keys it on the digest that actually
            // matches the stored object. The server-side dedup gate
            // (`precreate_dedup`) is the authoritative check just below at
            // Stage 4; this local guard is a fast fail-fast for the common
            // re-add-the-same-file case.
            if !task.force_upload
                && let Ok(Some(existing_id)) = self.db.find_by_hash(&hashes.blake3_hex).await
            {
                let _ = self.event_tx.send(UploadEvent::DuplicateDetected {
                    task_id: task.id.clone(),
                    existing_document_id: existing_id.clone(),
                });
                return Err(UploadError::Duplicate { existing_id });
            }
            // The in-flight sibling de-dup that used to sit here ran only for
            // rows that need rehashing. It now lives in the pre-create gate in
            // Stage 4 (`precreate_dedup`) so it guards *every* path to create —
            // happy path, auto-retry, and resume — not just rehashed rows.
        }
        // Set unconditionally above when `needs_rehash`, or carried on a
        // fully-hashed resumed row. An empty hash here would silently match
        // the wrong row in `find_by_hash` / `insert_file_hash`, so fail
        // loudly instead.
        let hash = task.hash.clone().expect("hash set in Stage 1");

        // Stage 2: Video re-check (skip when staging already populated
        // `video_info`). Defense-in-depth for legacy rows that were
        // staged before the server-side quality check existed —
        // failing here on an unplayable file keeps the bad file from
        // burning transcode CPU and upload bandwidth. We only run the
        // local atom walk; the full server check would re-run the
        // acceptance gate, which is wrong for a row the user has
        // already confirmed. `video_info` stays `None` for such rows
        // (the popover gets its data from the next quality check on
        // re-stage if the user wants it).
        // Materialize a plain `Option<VideoInfo>` for the transient transcode
        // pipeline below (`maybe_transcode` wants `&Option<VideoInfo>`). The
        // stored field is `Arc`-wrapped; one deep clone here is fine — this is
        // not a hot path and runs at most once per task.
        let video_info: Option<crate::models::VideoInfo> = if task.video_info.is_some() {
            task.video_info.as_deref().cloned()
        } else if task.mime_type.starts_with("video/") {
            self.update_state(task, UploadState::Validating).await;
            let path_buf = path.to_path_buf();
            match tokio::task::spawn_blocking(move || video_head::extract_atom_chunks(&path_buf))
                .await
            {
                Ok(Ok(_)) => None,
                Ok(Err(VideoValidationError::Unplayable { reason })) => {
                    return Err(UploadError::VideoUnplayable { reason });
                }
                Ok(Err(e)) => {
                    tracing::warn!("Atom walk failed for {}: {e}", task.filename);
                    None
                }
                Err(join_err) => {
                    tracing::error!("Atom walk task panicked for {}: {join_err}", task.filename);
                    None
                }
            }
        } else {
            None
        };

        // (Transcoded / desensitized temp copies are tracked on the `temp_guard`
        // declared at Stage 0 above, which also tracks the capture-tagged copy.)

        // Stage 2.5: Transcoding (user opt-in, video files only)
        let transcoded_path = self.maybe_transcode(task, path, &video_info).await?;
        temp_guard.track(transcoded_path.as_deref());

        // Stage 3: Data desensitization — REMOVED in this branch (PR: "retry QC on
        // transient 503 + remove the desensitize stage"). The desensitize module,
        // its `strip_metadata` engine flag, `video::metadata_needs_strip`, and the
        // `Desensitizing` state were all deleted, so there is no metadata-strip pass
        // before upload.
        //
        // Why it's gone (master's rationale, kept for the record): the strip pass
        // (`ffmpeg -map_metadata -1`) ALSO stripped the io.visionlab capture tags we
        // now embed before upload, and it was the dominant upload-time cost in user
        // logs (read-4GB + write-4GB-temp + hold-an-upload-slot per clip). master
        // had already disabled it unconditionally; this branch removes the scaffold
        // outright. If privacy stripping is reintroduced, it must re-embed the
        // capture metadata onto the final upload artifact AFTER the strip so the two
        // can coexist.
        let upload_path = transcoded_path.as_deref().unwrap_or(path);
        let upload_size = tokio::fs::metadata(upload_path)
            .await
            .map_err(|e| UploadError::from_source_io(e, upload_path))?
            .len();

        // Sanity check: if this task carries a recorded `transcoded_size`
        // (persisted when `maybe_transcode` finished in this run OR loaded
        // from SQLite on a resumed task) and we just picked the transcoded
        // file as the upload source, the two sizes must agree. A mismatch
        // means either the scratch file was rewritten between runs or the
        // DB row is stale — either way, the UI's "original → transcoded"
        // readout would misreport progress.
        if let (Some(recorded), true) = (task.transcoded_size, transcoded_path.as_deref().is_some())
            && recorded != upload_size
        {
            tracing::warn!(
                task_id = %task.id,
                recorded = recorded,
                actual = upload_size,
                "transcoded_size mismatch — resumed upload_size differs from recorded artifact size; syncing"
            );
            task.transcoded_size = Some(upload_size);
            let _ = self
                .db
                .update_upload_transcoded_size(&task.id, upload_size)
                .await;
            let _ = self.event_tx.send(UploadEvent::TranscodeCompleted {
                task_id: task.id.clone(),
                transcoded_size: upload_size,
            });
        }

        // Stage 4: Create document (skip if already has document_id)
        let doc_id = if let Some(ref doc_id) = task.document_id {
            tracing::info!("Resuming: document already created ({})", doc_id);
            doc_id.clone()
        } else {
            self.update_state(task, UploadState::Creating).await;
            // Compute the perceptual frames (cheap: <=5 keyframe seeks) on the
            // post-embed source so the created doc is persisted as a
            // near-duplicate match target AND the same frames feed the dedup
            // query below. Empty + omitted while `pdq::PDQ_ENABLED` is false.
            let pdq_frames = pdq::compute_pdq_frames(path).await;

            // INVARIANT: every `create_document` is immediately preceded by a
            // fresh dedup check — and this is now the SOLE dedup point. Staging
            // is QC-only; it no longer hashes or dedups, because the
            // capture-metadata embed rewrites the file after staging, so a
            // staging-time fingerprint could never match the stored object. The
            // digest + PDQ frames checked here are taken on the post-embed bytes
            // (Stage 1 rehashed `path`, the tagged artifact), so the dedup-time
            // signals are exactly the ones persisted on the created document.
            // This gate adopts the in-flight orphan a lost create-response left
            // behind (instead of minting a second document) and settles
            // `Rejected` when the content already exists (completed,
            // cross-tenant, or perceptual near-duplicate).
            match self.precreate_dedup(task, &hash, pdq_frames.clone()).await {
                PreCreate::Adopt(existing_id) => {
                    tracing::info!(
                        existing_id,
                        "pre-create dedup: adopting existing document instead of creating a duplicate"
                    );
                    task.document_id = Some(existing_id.clone());
                    self.db
                        .update_upload_document_id(&task.id, &existing_id)
                        .await?;
                    existing_id
                }
                PreCreate::AlreadyInFlight { existing_id } => {
                    let _ = self.event_tx.send(UploadEvent::DuplicateDetected {
                        task_id: task.id.clone(),
                        existing_document_id: existing_id.clone(),
                    });
                    return Err(UploadError::Duplicate { existing_id });
                }
                PreCreate::Rejected(reason) => {
                    tracing::info!(
                        reason,
                        "pre-create dedup: duplicate already exists; settling Rejected without creating"
                    );
                    self.settle_post_hash(
                        &task.id,
                        UploadState::Rejected,
                        Vec::new(),
                        vec![reason],
                    )
                    .await;
                    return Ok(());
                }
                PreCreate::Create => {
                    let doc = self
                        .api
                        .create_document(
                            &task.tenant_id,
                            &task.project_id,
                            &CreateDocumentRequest {
                                collection: "documents".to_string(),
                                description: task.filename.clone(),
                                metadata: CreateDocumentMeta {
                                    filename: task.filename.clone(),
                                    size: Some(upload_size as i64),
                                    mime_type: task.mime_type.clone(),
                                },
                                model_name: None,
                                folder: None,
                                // Top-level on the request, NOT inside metadata —
                                // the backend persists this to a dedicated
                                // `document_digests.original_digest` JSONB column
                                // with strict regex validation per leg; placing
                                // it under `metadata` would silently drop the
                                // value because `DocumentMeta` does not declare
                                // these keys. All three legs come from the same
                                // single I/O pass at staging time (or the Stage
                                // 1 rehash fallback above) so they're guaranteed
                                // self-consistent.
                                digest: Some(Digest {
                                    md5: task.source_md5.clone(),
                                    crc32c: task.source_crc32c.clone(),
                                    sha256_head_256kib: task.source_sha256_head_256kib.clone(),
                                }),
                                // Persisted to `public.document_frame_hashes` so
                                // this upload becomes a near-duplicate match
                                // target. Omitted while PDQ is disabled / nothing
                                // decoded.
                                pdq_frames: (!pdq_frames.is_empty()).then_some(pdq_frames),
                            },
                        )
                        .await?;
                    task.document_id = Some(doc.id.clone());
                    self.db.update_upload_document_id(&task.id, &doc.id).await?;
                    doc.id
                }
            }
        };

        // Stage 5 + 6 (preferred): parallel multipart (XML MPU) path.
        //
        // Only attempted for a fresh upload (no prior resumable session) and
        // only on the GCS backend. MPU resume across restart is out of scope
        // (P1): a task that already carries a resumable `session_id` stays on
        // the resumable path below so its resume keeps working. If the backend
        // lacks the feature (`get_multipart_upload` → 404 → `Ok(None)`) we fall
        // through to the resumable path — a safe rollout with no client change.
        let backend_is_gcs = matches!(self.storage.as_ref(), StorageBackend::Gcs(_));
        if backend_is_gcs && task.session_id.is_none() {
            // Resume path: a persisted `mpu_upload_id` means a prior MPU attempt
            // was interrupted (app restart / crash). Ask the backend which parts
            // already landed on GCS (ListParts) and upload only the rest.
            if let Some(upload_id) = task.mpu_upload_id.clone()
                && let Some(resume) = self
                    .api
                    .resume_multipart_upload(
                        &task.tenant_id,
                        &task.project_id,
                        &doc_id,
                        &upload_id,
                        upload_size as i64,
                    )
                    .await?
            {
                return self
                    .do_mpu_resume(
                        task,
                        resume,
                        upload_path,
                        upload_size,
                        &hash,
                        &transcoded_path,
                    )
                    .await;
            }
            // The MPU is gone server-side. Before treating that as "re-upload the
            // whole file from part 1", check whether the blob is already durable
            // (the upload actually finished and the MPU was just reaped/expired).
            // If so, finalize without re-uploading. Only meaningful when we held a
            // persisted upload id — a fresh task has nothing that could be done.
            if task.mpu_upload_id.is_some()
                && self
                    .document_blob_complete(&task.tenant_id, &task.project_id, &doc_id)
                    .await
            {
                tracing::info!(
                    doc_id = %doc_id,
                    "resume: blob already durable on server; finalizing without re-upload"
                );
                task.bytes_uploaded = upload_size;
                let _ = self.db.update_upload_progress(&task.id, upload_size).await;
                return self
                    .finalize_after_upload(task, &hash, &transcoded_path)
                    .await;
            }
            // Either no persisted upload, or the persisted one is gone (404 /
            // NoSuchUpload — expired or already completed). If we held a stale
            // id, best-effort abort it and clear it before initiating fresh.
            if let Some(stale) = task.mpu_upload_id.clone() {
                let _ = self
                    .api
                    .abort_multipart_upload(&task.tenant_id, &task.project_id, &doc_id, &stale)
                    .await;
                task.mpu_upload_id = None;
                self.db.update_upload_mpu_upload_id(&task.id, None).await?;
            }

            // Fresh MPU.
            let plan = self
                .api
                .get_multipart_upload(
                    &task.tenant_id,
                    &task.project_id,
                    &doc_id,
                    upload_size as i64,
                )
                .await?;
            if let Some(plan) = plan {
                return self
                    .do_mpu_upload(
                        task,
                        plan,
                        upload_path,
                        upload_size,
                        &hash,
                        &transcoded_path,
                    )
                    .await;
            }
            tracing::info!("multipart unavailable — using resumable upload path");
        }

        // Stage 5: Initiate resumable session (skip if already has session_id)
        let session = if let Some(ref sid) = task.session_id {
            tracing::info!("Resuming: session already initiated, querying progress");
            let s = storage::UploadSession {
                session_id: sid.clone(),
                total_size: upload_size,
                bytes_confirmed: task.bytes_uploaded,
            };
            // Query server for actual progress
            match self.storage.query_progress(&s).await {
                Ok(confirmed) => {
                    task.bytes_uploaded = confirmed;
                    tracing::info!("Resuming from byte {confirmed}/{upload_size}");
                }
                Err(e) => {
                    tracing::warn!("Could not query progress, re-initiating session: {e}");
                    // Re-initiating restarts the upload from byte 0. First check
                    // whether the blob is already durable server-side (the upload
                    // finished and only the resumable session handle expired) — if
                    // so, finalize without re-sending the whole file.
                    if self
                        .document_blob_complete(&task.tenant_id, &task.project_id, &doc_id)
                        .await
                    {
                        tracing::info!(
                            doc_id = %doc_id,
                            "resume: blob already durable on server; finalizing without re-upload"
                        );
                        task.bytes_uploaded = upload_size;
                        let _ = self.db.update_upload_progress(&task.id, upload_size).await;
                        return self
                            .finalize_after_upload(task, &hash, &transcoded_path)
                            .await;
                    }
                    // Session expired — get new URL and re-initiate
                    let signed_url = self
                        .api
                        .get_upload_url(&task.tenant_id, &task.project_id, &doc_id)
                        .await?;
                    let new_session = self
                        .storage
                        .initiate_upload(&signed_url.url, &task.mime_type, upload_size)
                        .await?;
                    task.session_id = Some(new_session.session_id.clone());
                    task.bytes_uploaded = 0;
                    self.db
                        .update_upload_session_id(&task.id, &new_session.session_id)
                        .await?;
                    return self
                        .do_upload(
                            task,
                            &new_session,
                            upload_path,
                            upload_size,
                            &hash,
                            &transcoded_path,
                        )
                        .await;
                }
            }
            s
        } else {
            let signed_url = self
                .api
                .get_upload_url(&task.tenant_id, &task.project_id, &doc_id)
                .await?;
            let session = self
                .storage
                .initiate_upload(&signed_url.url, &task.mime_type, upload_size)
                .await?;
            task.session_id = Some(session.session_id.clone());
            self.db
                .update_upload_session_id(&task.id, &session.session_id)
                .await?;
            session
        };

        // Stage 6: Chunked resumable upload (resumes from bytes_uploaded offset)
        self.do_upload(
            task,
            &session,
            upload_path,
            upload_size,
            &hash,
            &transcoded_path,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn do_upload(
        &self,
        task: &mut UploadTask,
        session: &storage::UploadSession,
        upload_path: &std::path::Path,
        upload_size: u64,
        hash: &str,
        transcoded_path: &Option<PathBuf>,
    ) -> Result<(), UploadError> {
        self.update_state(task, UploadState::Uploading).await;

        let event_tx = self.event_tx.clone();
        let task_id = task.id.clone();
        let on_progress: storage::ProgressFn = Box::new(move |uploaded, total| {
            let _ = event_tx.send(UploadEvent::Progress {
                task_id: task_id.clone(),
                bytes_uploaded: uploaded,
                total_bytes: total,
            });
        });

        // Scale the resumable chunk size to the file. A fixed small chunk makes a
        // multi-GB upload pay a per-chunk round-trip + TCP slow-start penalty
        // hundreds of times and never saturate the link; `self.chunk_size` (the
        // config `chunk_size_mb`) acts as the floor. See `storage::pick_chunk_size`.
        let chunk_size = storage::pick_chunk_size(upload_size, self.chunk_size);
        // Every upload uses the full per-chunk retry budget. Bounded
        // concurrency (the `upload_semaphore`) keeps a dead file from starving
        // the others, so there is no fast-fail mode — flaky-network resilience
        // comes from the retry budget + capped backoff, not from serializing.
        let max_retries = storage::DEFAULT_MAX_RETRIES;
        tracing::info!(
            filename = %task.filename,
            upload_size,
            chunk_size,
            chunk_count = upload_size.div_ceil(chunk_size.max(1)),
            max_retries,
            "upload: selected dynamic chunk size",
        );

        let cancel = self.cancel_flag_for(&task.id);
        let confirmed = storage::upload_file_chunked(
            self.storage.as_ref(),
            session,
            upload_path,
            task.bytes_uploaded,
            chunk_size,
            max_retries,
            &on_progress,
            &cancel,
        )
        .await?;

        task.bytes_uploaded = confirmed;
        let _ = self.db.update_upload_progress(&task.id, confirmed).await;

        self.finalize_after_upload(task, hash, transcoded_path)
            .await
    }

    /// Shared Stage 7 + completion tail for both the resumable
    /// (`do_upload`) and the parallel multipart (`do_mpu_upload`) paths:
    /// verify the document on the server, mark the task completed, record the
    /// file hash for future dedup, and clean up temp + (optionally) original
    /// files. The bytes have already been fully uploaded by the caller.
    async fn finalize_after_upload(
        &self,
        task: &mut UploadTask,
        hash: &str,
        transcoded_path: &Option<PathBuf>,
    ) -> Result<(), UploadError> {
        let doc_id = task.document_id.clone().unwrap_or_default();

        // Verify
        self.update_state(task, UploadState::Verifying).await;
        self.api
            .verify_upload(&task.tenant_id, &task.project_id, &doc_id, 10)
            .await?;

        // Complete — one guarded, idempotent write that also folds in the
        // retry-count reset (see `db::settle_completed`). Because completion is a
        // fact about the server (bytes durable + verified) it is not keyed on any
        // owner, and because the reset rides the same guarded UPDATE, a lagging
        // duplicate worker can neither revert this terminal state nor re-arm the
        // give-up budget. Mirror the transition into the UI + the in-memory task.
        let _ = self.db.settle_completed(&task.id).await;
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task.id.clone(),
            state: UploadState::Completed,
        });
        task.state = UploadState::Completed;
        let _ = self.event_tx.send(UploadEvent::Completed {
            task_id: task.id.clone(),
        });

        // Record hash for future dedup
        let _ = self
            .db
            .insert_file_hash(
                hash,
                &task.filename,
                task.size,
                &task.tenant_id,
                &task.project_id,
                &doc_id,
            )
            .await;

        // Clean up the transcode temp file
        if let Some(tp) = transcoded_path {
            ffmpeg_util::cleanup_temp_file(tp);
        }

        // Auto-clean original file
        if self.auto_clean()
            && let Err(e) = tokio::fs::remove_file(&task.local_path).await
        {
            tracing::warn!("Failed to auto-clean {}: {e}", task.local_path);
        }

        Ok(())
    }

    /// Stage 6 for the parallel multipart (XML MPU) path. Maps the backend
    /// plan into the storage layer's `PartUrl`s, drives the bounded-parallel
    /// part PUTs, completes the MPU with the collected ETags, then runs the
    /// shared Stage 7 + completion tail.
    ///
    /// The `uploadId` is persisted (`mpu_upload_id`) BEFORE any part PUT, so an
    /// app restart mid-upload resumes this exact MPU (see the resume branch in
    /// `process_task`) instead of re-initiating. Consequently a terminal
    /// failure here does NOT abort the server-side MPU — the already-uploaded
    /// parts are left intact so a retry/restart can resume them; the error is
    /// propagated and the task lands in `Failed` with its `mpu_upload_id` kept.
    /// (Truly-abandoned uploads are reaped by the bucket's
    /// AbortIncompleteMultipartUpload lifecycle rule.)
    #[allow(clippy::too_many_arguments)]
    async fn do_mpu_upload(
        &self,
        task: &mut UploadTask,
        plan: crate::models::MultipartPlan,
        upload_path: &std::path::Path,
        upload_size: u64,
        hash: &str,
        transcoded_path: &Option<PathBuf>,
    ) -> Result<(), UploadError> {
        self.update_state(task, UploadState::Uploading).await;

        let doc_id = task.document_id.clone().unwrap_or_default();
        let upload_id = plan.upload_id.clone();

        // Persist the uploadId before any part PUT so a crash/restart mid-upload
        // resumes this MPU (ListParts → upload only missing parts) instead of
        // re-initiating from zero and orphaning these parts on GCS.
        task.mpu_upload_id = Some(upload_id.clone());
        self.db
            .update_upload_mpu_upload_id(&task.id, Some(&upload_id))
            .await?;

        let part_size = plan.part_size.max(0) as u64;
        let parts: Vec<storage::PartUrl> = plan
            .parts
            .into_iter()
            .map(|p| storage::PartUrl {
                part_number: p.part_number.max(0) as u32,
                url: p.url,
            })
            .collect();

        let event_tx = self.event_tx.clone();
        let task_id = task.id.clone();
        let on_progress: storage::ProgressFn = Box::new(move |uploaded, total| {
            let _ = event_tx.send(UploadEvent::Progress {
                task_id: task_id.clone(),
                bytes_uploaded: uploaded,
                total_bytes: total,
            });
        });
        let on_retry = self.mpu_retry_notifier(&task.id);

        let max_retries = storage::DEFAULT_MAX_RETRIES;
        tracing::info!(
            filename = %task.filename,
            upload_size,
            part_size,
            parts = parts.len(),
            max_retries,
            "upload: parallel multipart path",
        );

        // Drive the parallel part PUTs, then complete. A failure here is NOT
        // aborted — the persisted `mpu_upload_id` lets a retry/restart resume
        // the already-uploaded parts (see this fn's doc comment).
        let cancel = self.cancel_flag_for(&task.id);
        self.run_mpu_parts_and_complete(
            &task.tenant_id,
            &task.project_id,
            &doc_id,
            &upload_id,
            upload_path,
            &parts,
            part_size,
            upload_size,
            &[],
            max_retries,
            &on_progress,
            Some(&on_retry),
            &cancel,
        )
        .await?;

        task.bytes_uploaded = upload_size;
        let _ = self.db.update_upload_progress(&task.id, upload_size).await;

        self.finalize_after_upload(task, hash, transcoded_path)
            .await
    }

    /// Stage 6 for a RESUMED parallel multipart upload (app restart / retry
    /// with a persisted `mpu_upload_id`). The backend already ran GCS ListParts
    /// and split the layout into `resume.parts` (still missing, freshly signed)
    /// and `resume.completed_parts` (already durable on GCS, with ETags). Uploads
    /// only the missing parts, folds in the completed ETags, completes, then runs
    /// the shared Stage 7 tail. A transient failure is NOT aborted — the kept
    /// `mpu_upload_id` lets the next retry/restart resume again.
    #[allow(clippy::too_many_arguments)]
    async fn do_mpu_resume(
        &self,
        task: &mut UploadTask,
        resume: crate::models::MultipartResumePlan,
        upload_path: &std::path::Path,
        upload_size: u64,
        hash: &str,
        transcoded_path: &Option<PathBuf>,
    ) -> Result<(), UploadError> {
        self.update_state(task, UploadState::Uploading).await;

        let doc_id = task.document_id.clone().unwrap_or_default();
        let upload_id = resume.upload_id.clone();
        let part_size = resume.part_size.max(0) as u64;
        let parts: Vec<storage::PartUrl> = resume
            .parts
            .into_iter()
            .map(|p| storage::PartUrl {
                part_number: p.part_number.max(0) as u32,
                url: p.url,
            })
            .collect();
        let completed: Vec<(u32, String)> = resume
            .completed_parts
            .into_iter()
            .map(|p| (p.part_number.max(0) as u32, p.etag))
            .collect();

        // Reconcile the resumed plan against the local file: missing + done must
        // cover exactly the deterministic part count. A mismatch (only possible
        // if the MPU was created against a different size) can't be resumed —
        // abandon the stale upload so the retry starts fresh instead of looping.
        let expected_parts = if part_size == 0 {
            0
        } else {
            upload_size.div_ceil(part_size)
        };
        let total_parts = (parts.len() + completed.len()) as u64;
        if part_size == 0 || total_parts != expected_parts {
            let _ = self
                .api
                .abort_multipart_upload(&task.tenant_id, &task.project_id, &doc_id, &upload_id)
                .await;
            task.mpu_upload_id = None;
            self.db.update_upload_mpu_upload_id(&task.id, None).await?;
            return Err(UploadError::MpuResumeFailed {
                upload_id,
                reason: format!(
                    "resumed plan covers {total_parts} parts, expected {expected_parts} for {upload_size}B at {part_size}B/part"
                ),
            });
        }

        // Seed progress with the bytes already durable on GCS so the UI shows
        // resumed progress instead of restarting at 0%. Approximate (per-part
        // sizes aren't carried), capped at the file size; the part PUTs add the
        // remainder on top via the offset closure.
        let already_bytes = (completed.len() as u64)
            .saturating_mul(part_size)
            .min(upload_size);
        let event_tx = self.event_tx.clone();
        let task_id = task.id.clone();
        let on_progress: storage::ProgressFn = Box::new(move |uploaded, total| {
            let _ = event_tx.send(UploadEvent::Progress {
                task_id: task_id.clone(),
                bytes_uploaded: already_bytes.saturating_add(uploaded).min(total),
                total_bytes: total,
            });
        });
        let on_retry = self.mpu_retry_notifier(&task.id);

        let max_retries = storage::DEFAULT_MAX_RETRIES;
        tracing::info!(
            filename = %task.filename,
            upload_size,
            part_size,
            remaining = parts.len(),
            completed = completed.len(),
            max_retries,
            "upload: resuming parallel multipart",
        );

        let cancel = self.cancel_flag_for(&task.id);
        self.run_mpu_parts_and_complete(
            &task.tenant_id,
            &task.project_id,
            &doc_id,
            &upload_id,
            upload_path,
            &parts,
            part_size,
            upload_size,
            &completed,
            max_retries,
            &on_progress,
            Some(&on_retry),
            &cancel,
        )
        .await?;

        task.bytes_uploaded = upload_size;
        let _ = self.db.update_upload_progress(&task.id, upload_size).await;

        self.finalize_after_upload(task, hash, transcoded_path)
            .await
    }

    /// Build the per-part on-retry callback for a multipart upload of `task_id`.
    ///
    /// Each failed-and-retryable part attempt fires
    /// [`UploadEvent::PartRetrying`] with the attempt count, which the UI turns
    /// into an event-driven "connection stalled — retrying" hint on the row —
    /// no byte-progress timeout. Returned as an [`Arc`] so the same callback is
    /// shared across the parallel per-part tasks (see `storage::RetryFn`). Built
    /// identically for the fresh and resume MPU paths.
    fn mpu_retry_notifier(&self, task_id: &str) -> Arc<storage::RetryFn> {
        let event_tx = self.event_tx.clone();
        let task_id = task_id.to_string();
        Arc::new(Box::new(move |attempt: u32| {
            let _ = event_tx.send(UploadEvent::PartRetrying {
                task_id: task_id.clone(),
                attempt,
            });
        }) as storage::RetryFn)
    }

    /// Inner helper: upload `parts` in parallel and complete the MPU. Shared by
    /// the fresh (`do_mpu_upload`) and resume (`do_mpu_resume`) paths.
    ///
    /// `already_completed` carries parts that were durable on GCS before this
    /// attempt (the resume case; empty `&[]` for a fresh upload). Their ETags
    /// are folded together with the freshly-uploaded ones so the completion
    /// body lists the full part set GCS expects.
    #[allow(clippy::too_many_arguments)]
    async fn run_mpu_parts_and_complete(
        &self,
        tenant_id: &str,
        project_id: &str,
        doc_id: &str,
        upload_id: &str,
        upload_path: &std::path::Path,
        parts: &[storage::PartUrl],
        part_size: u64,
        upload_size: u64,
        already_completed: &[(u32, String)],
        max_retries: u32,
        on_progress: &storage::ProgressFn,
        on_retry: Option<&Arc<storage::RetryFn>>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), UploadError> {
        let StorageBackend::Gcs(gcs) = self.storage.as_ref() else {
            // MPU is GCS-only; the S3 path never reaches here (it has no
            // multipart-upload-url endpoint). Treat as a structured API error
            // rather than panicking.
            return Err(UploadError::Api {
                status: 500,
                message: "multipart upload requested on a non-GCS backend".to_string(),
            });
        };
        let collected = gcs
            .upload_file_mpu(
                upload_path,
                parts,
                part_size,
                upload_size,
                max_retries,
                on_progress,
                on_retry,
                cancel,
            )
            .await?;

        // Fold the freshly-uploaded part ETags together with any parts that
        // were already durable on GCS before this attempt (resume path), then
        // complete with the full ascending set.
        let mut all_parts: Vec<(u32, String)> = already_completed.to_vec();
        all_parts.extend(collected);
        all_parts.sort_by_key(|(part_number, _)| *part_number);
        self.api
            .complete_multipart_upload(tenant_id, project_id, doc_id, upload_id, &all_parts)
            .await
    }

    /// Cooperatively pause an in-flight upload. Meaningful ONLY for a row in
    /// `Uploading` (the UI surfaces Pause only there; this re-checks to guard
    /// misuse). Trips the task's cancel flag; the upload worker observes it at the
    /// next chunk/part boundary, returns [`UploadError::Cancelled`], and the
    /// dispatch layer settles the row to `Paused` (guarded) + emits `StateChanged`.
    /// Already-uploaded bytes/parts stay durable server-side, so a later resume
    /// re-sends only the remainder. Returns `Ok(false)` (no-op) if the row is not
    /// `Uploading` or no worker is currently driving it. Resume is the existing
    /// `Paused -> Pending` path via the UI's `on_resume -> dispatch_one`.
    pub async fn pause_task(&self, task_id: &str) -> Result<bool, DbError> {
        let Some(task) = self.db.get_upload_by_id(task_id).await? else {
            return Ok(false);
        };
        if task.state != UploadState::Uploading {
            return Ok(false);
        }
        let tripped = self
            .cancels
            .lock()
            .expect("cancels lock poisoned")
            .get(task_id)
            .map(|flag| flag.store(true, std::sync::atomic::Ordering::Relaxed))
            .is_some();
        Ok(tripped)
    }

    /// Settle a worker that returned `Cancelled` (a user pause) to `Paused` — a
    /// guarded write (only from `Uploading`) plus the `StateChanged` event so the
    /// UI reflects engine truth. Distinct from the failure path: a pause is not a
    /// failure and must never enter the auto-retry sweep.
    async fn settle_cancelled_as_paused(&self, task_id: &str) {
        if let Ok(true) = self.db.settle_paused(task_id).await {
            let _ = self.event_tx.send(UploadEvent::StateChanged {
                task_id: task_id.to_string(),
                state: UploadState::Paused,
            });
        }
    }

    /// Resume pending uploads from database
    pub async fn resume_pending(self: &Arc<Self>) -> Result<(), DbError> {
        let tasks = self.db.get_pending_uploads().await?;
        tracing::info!("Resuming {} pending uploads", tasks.len());

        for mut task in tasks {
            let engine = Arc::clone(self);
            let sem = Arc::clone(&self.upload_semaphore);

            tokio::spawn(async move {
                // Bound concurrency on resume too, so a backlog doesn't fan out
                // to one connection per pending file.
                let _permit = sem.acquire().await.expect("semaphore closed");
                // Single-flight: never run a row the retry loop (or a duplicate
                // resume dispatch) is already driving.
                let Some(_flight) = engine.try_enter_flight(&task.id) else {
                    return;
                };
                if let Some(ref sid) = task.session_id {
                    let session = storage::UploadSession {
                        session_id: sid.clone(),
                        total_size: task.size,
                        bytes_confirmed: task.bytes_uploaded,
                    };
                    match engine.storage.query_progress(&session).await {
                        Ok(confirmed) => task.bytes_uploaded = confirmed,
                        Err(e) => tracing::warn!("Could not query progress: {e}"),
                    }
                }

                match engine.process_task(&mut task).await {
                    Ok(()) => tracing::info!("Upload completed: {}", task.filename),
                    // User pause: settle to Paused, not Failed.
                    Err(UploadError::Cancelled) => {
                        engine.settle_cancelled_as_paused(&task.id).await;
                    }
                    Err(e) => {
                        e.log(format_args!("Upload of {}", task.filename));
                        let _ = engine
                            .db
                            .settle_failure(&task.id, UploadState::Failed, &e.to_string())
                            .await;
                        let _ = engine.event_tx.send(UploadEvent::Failed {
                            task_id: task.id,
                            error: e.to_string(),
                        });
                    }
                }
            });
        }

        Ok(())
    }

    /// Spawn a background task that auto-retries failed uploads when the network
    /// recovers. Polls every 30s, but each failed file is re-queued on its own
    /// exponential backoff ([`auto_retry_backoff`]) and only as many as there
    /// are free upload slots are launched per tick — so one flaky-network window
    /// no longer re-queues every failed file every 30s (the storm that kept
    /// dozens of files cycling through PENDING all night and starved the slots).
    ///
    /// Two responsibilities beyond re-queueing:
    /// * **Give-up is durable.** The cap and backoff are driven from the
    ///   *persisted* `task.retry_count` (incremented in the DB before each
    ///   retry), not the loop-local `retry_state`. That map is kept only for
    ///   backoff *pacing* (last-attempt instant); losing it (restart) no longer
    ///   resets the cap, and the old `retain(live)` prune can no longer defeat
    ///   it — the count lives in SQLite and only [`Self::retry_task`] moves it.
    /// * **Network health.** Each tick times the (proxy-aware) probe and
    ///   classifies a [`NetworkReading`]; a [`UploadEvent::NetworkQuality`] is
    ///   emitted only when the tier changes (debounced), and the latest tier is
    ///   handed to `retry_task` so a give-up on a weak link gets actionable copy.
    pub fn spawn_auto_retry(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            // Loop-local backoff pacing: task_id -> last-attempt instant. Cap
            // and attempt count come from the DB, so this map is pacing-only and
            // its loss (restart) is harmless.
            let mut last_attempt: std::collections::HashMap<String, std::time::Instant> =
                std::collections::HashMap::new();
            // Loop-local health debounce state (consecutive failures + last tier
            // emitted). No shared mutable global state — a Copy value threaded
            // through the tick.
            let mut probe_state = ProbeState::default();
            loop {
                interval.tick().await;
                let reading = probe_network(&engine.probe_client, &mut probe_state).await;
                engine.emit_health_on_change(&mut probe_state, reading);
                if matches!(reading.health, NetworkHealth::Offline) {
                    // Storage unreachable — don't churn retries into a wall.
                    continue;
                }
                engine.run_retry_tick(&mut last_attempt, reading).await;
            }
        })
    }

    /// Emit a [`UploadEvent::NetworkQuality`] only when the tier changed since
    /// the last emit (debounce), then record the new tier. Keeps the UI chip and
    /// banner reacting to transitions, not to every 30s tick.
    fn emit_health_on_change(&self, state: &mut ProbeState, reading: NetworkReading) {
        if state.last_emitted == Some(reading.health) {
            return;
        }
        state.last_emitted = Some(reading.health);
        let _ = self.event_tx.send(UploadEvent::NetworkQuality(reading));
    }

    /// One auto-retry pass: load retryable failures, launch as many as there are
    /// free upload slots (backoff-paced from the persisted count), and record the
    /// attempt instant for pacing. `reading` is forwarded to each `retry_task` so
    /// a give-up can compose weak-network copy.
    async fn run_retry_tick(
        self: &Arc<Self>,
        last_attempt: &mut std::collections::HashMap<String, std::time::Instant>,
        reading: NetworkReading,
    ) {
        let failed = match self.db.get_failed_retryable().await {
            Ok(tasks) => tasks,
            Err(_) => return,
        };
        // Drop pacing entries for tasks no longer failing, so a file that fails
        // again later starts its backoff fresh.
        let live: std::collections::HashSet<&str> = failed.iter().map(|t| t.id.as_str()).collect();
        last_attempt.retain(|id, _| live.contains(id.as_str()));

        // Only launch as many retries as there are free upload slots: a backlog
        // backs off in the queue instead of all flipping to PENDING and
        // contending for the (default 2) upload permits.
        let free = self.upload_semaphore.available_permits();
        if free == 0 {
            return;
        }
        let now = std::time::Instant::now();
        let to_launch: Vec<UploadTask> = failed
            .into_iter()
            .filter(|t| auto_retry_due(t.retry_count, last_attempt.get(&t.id).copied(), now))
            .take(free)
            .collect();
        if to_launch.is_empty() {
            return;
        }
        tracing::info!(
            "Auto-retrying {} failed uploads (backoff-paced, {free} slots free)",
            to_launch.len()
        );
        for task in to_launch {
            last_attempt.insert(task.id.clone(), now);
            // Persist the attempt BEFORE spawning: the count is durable (survives
            // restart) and immune to any in-process bookkeeping loss.
            if let Err(e) = self.db.increment_retry_count(&task.id).await {
                tracing::warn!("Failed to persist retry_count for {}: {e}", task.id);
            }
            let attempt = task.retry_count + 1;
            let eng = Arc::clone(self);
            let sem = Arc::clone(&self.upload_semaphore);
            tokio::spawn(Self::retry_task(eng, sem, task, attempt, reading));
        }
    }

    /// Run one auto-retry attempt for `task`. `attempt` is the 1-based, already
    /// persisted retry number (`retry_count` after the increment). On failure,
    /// decide give-up vs. keep-failing from that persisted count: at
    /// [`AUTO_RETRY_MAX_ATTEMPTS`] the row settles [`UploadState::GaveUp`] with a
    /// terminal message composed from `reading` (weak-network copy when the link
    /// is Weak/Offline and the error is transport-transient); otherwise it stays
    /// [`UploadState::Failed`] for the next tick.
    async fn retry_task(
        eng: Arc<Self>,
        sem: Arc<Semaphore>,
        mut task: UploadTask,
        attempt: u32,
        reading: NetworkReading,
    ) {
        let _permit = sem.acquire().await.expect("semaphore closed");
        // Single-flight: never start a retry for a row already being driven by a
        // resume worker or a concurrent dispatch of the same id.
        let Some(_flight) = eng.try_enter_flight(&task.id) else {
            return;
        };
        // Surface the retry so the row reads "retrying (attempt N)" instead of a
        // silent PENDING while it waits for / holds one of the upload slots.
        let _ = eng.event_tx.send(UploadEvent::Retrying {
            task_id: task.id.clone(),
            attempt,
        });
        let _ = eng
            .db
            .update_upload_state(&task.id, UploadState::Pending, None)
            .await;
        let _ = eng.event_tx.send(UploadEvent::StateChanged {
            task_id: task.id.clone(),
            state: UploadState::Pending,
        });
        task.state = UploadState::Pending;
        match eng.process_task(&mut task).await {
            Ok(()) => tracing::info!("Auto-retry succeeded: {}", task.filename),
            // User pause during an auto-retry attempt: hold, don't count as failure.
            Err(UploadError::Cancelled) => eng.settle_cancelled_as_paused(&task.id).await,
            Err(e) => eng.settle_retry_failure(&task, attempt, &e, reading).await,
        }
    }

    /// Persist + announce the terminal state after an auto-retry attempt failed.
    /// Give up (settle [`UploadState::GaveUp`]) once `attempt` has reached the
    /// cap; otherwise keep the row [`UploadState::Failed`] so the next tick can
    /// retry it. The give-up message is composed by [`compose_give_up_message`].
    async fn settle_retry_failure(
        &self,
        task: &UploadTask,
        attempt: u32,
        err: &UploadError,
        reading: NetworkReading,
    ) {
        if attempt >= AUTO_RETRY_MAX_ATTEMPTS {
            let message = compose_give_up_message(attempt, err, reading);
            tracing::warn!("Giving up auto-retry for {}: {err}", task.filename);
            let _ = self
                .db
                .settle_failure(&task.id, UploadState::GaveUp, &message)
                .await;
            let _ = self.event_tx.send(UploadEvent::Failed {
                task_id: task.id.clone(),
                error: message,
            });
            let _ = self.event_tx.send(UploadEvent::StateChanged {
                task_id: task.id.clone(),
                state: UploadState::GaveUp,
            });
            return;
        }
        tracing::warn!("Auto-retry failed for {}: {err}", task.filename);
        let _ = self
            .db
            .settle_failure(&task.id, UploadState::Failed, &err.to_string())
            .await;
        let _ = self.event_tx.send(UploadEvent::Failed {
            task_id: task.id.clone(),
            error: err.to_string(),
        });
    }

    /// Returns Ok(Some(path)) if transcoded, Ok(None) if not requested, Err if failed.
    async fn maybe_transcode(
        &self,
        task: &mut UploadTask,
        path: &Path,
        video_info: &Option<crate::models::VideoInfo>,
    ) -> Result<Option<PathBuf>, UploadError> {
        if !task.transcode || !task.mime_type.starts_with("video/") {
            return Ok(None);
        }
        let Some(info) = video_info else {
            return Ok(None);
        };

        // Upscale guard: if the source is already at or below target on all
        // axes, transcoding only costs CPU/storage. The UI hides the toggle
        // in this case, but we short-circuit here too so mid-session config
        // changes can't re-open the gap.
        if !video::transcode_would_help(info, &self.transcode_config) {
            tracing::info!(
                "Skipping transcode for {}: source already at/below targets",
                task.filename,
            );
            return Ok(None);
        }

        self.update_state(task, UploadState::Transcoding).await;
        let input = path.to_path_buf();
        let info_clone = info.clone();
        let config = self.transcode_config.clone();
        let event_tx = self.event_tx.clone();
        let task_id = task.id.clone();

        let progress_cb = move |done: u64, total: u64| {
            let pct = if total > 0 {
                done as f32 / total as f32 * 100.0
            } else {
                0.0
            };
            let _ = event_tx.send(UploadEvent::TranscodeProgress {
                task_id: task_id.clone(),
                percent: pct,
            });
        };

        let result = tokio::task::spawn_blocking(move || {
            transcode::transcode_video(&input, &info_clone, &config, &progress_cb)
        })
        .await
        .map_err(|e| UploadError::Io(std::io::Error::other(format!("Transcode panicked: {e}"))))?
        .map_err(|e| UploadError::Io(std::io::Error::other(format!("Transcode failed: {e}"))))?;

        tracing::info!(
            "Transcoded {}: {:.1}MB → {:.1}MB",
            task.filename,
            result.original_size as f64 / 1_048_576.0,
            result.transcoded_size as f64 / 1_048_576.0,
        );
        task.transcoded_size = Some(result.transcoded_size);
        let _ = self
            .db
            .update_upload_transcoded_size(&task.id, result.transcoded_size)
            .await;
        let _ = self.event_tx.send(UploadEvent::TranscodeCompleted {
            task_id: task.id.clone(),
            transcoded_size: result.transcoded_size,
        });
        Ok(Some(result.output_path))
    }

    async fn update_state(&self, task: &mut UploadTask, state: UploadState) {
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task.id.clone(),
            state: state.clone(),
        });
        let _ = self
            .db
            .update_upload_state(&task.id, state.clone(), None)
            .await;
        task.state = state;
    }
}

/// Base delay before auto-retrying a failed upload — one `spawn_auto_retry`
/// tick.
const AUTO_RETRY_BASE: std::time::Duration = std::time::Duration::from_secs(30);
/// Plateau for the auto-retry backoff: a long-dead file is re-checked every
/// 15 min rather than hammered every 30s or abandoned outright.
const AUTO_RETRY_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(900);
/// Give up auto-retrying a file after this many attempts; it moves to the
/// terminal [`UploadState::GaveUp`] state for the user to retry manually.
/// Bounds a permanently-dead file instead of re-queueing it forever. The
/// count is now the *persisted* `retry_count` (incremented in the DB before
/// each retry — see [`UploadEngine::run_retry_tick`]); the shipped
/// `retry_count < 10` guard never fired because `retry_count` was never
/// incremented, and the in-process counter was pruned by the `retain(live)`
/// call before it could reach the cap.
pub(crate) const AUTO_RETRY_MAX_ATTEMPTS: u32 = 10;

/// Per-task backoff before a failed upload is auto-retried again, doubling with
/// each prior auto-retry: 30s, 1m, 2m, 4m, 8m, … capped at 15m. Without this,
/// `spawn_auto_retry` re-queued every failed file on every 30s tick — one
/// flaky-network window turned into an all-night storm that kept dozens of
/// files cycling through PENDING and starved the upload slots.
fn auto_retry_backoff(retry_count: u32) -> std::time::Duration {
    let shift = retry_count.min(5);
    AUTO_RETRY_BASE
        .saturating_mul(1 << shift)
        .min(AUTO_RETRY_MAX_BACKOFF)
}

/// Whether a failed task whose last auto-retry was `since_last_attempt` ago is
/// due for another, given how many times it has already been auto-retried.
fn should_auto_retry(retry_count: u32, since_last_attempt: std::time::Duration) -> bool {
    since_last_attempt >= auto_retry_backoff(retry_count)
}

/// Decide whether a failed task is due for an auto-retry this tick.
///
/// `retry_count` is the task's *persisted* auto-retry count (the give-up axis);
/// `last_attempt` is the loop-local instant of its previous auto-retry, if any
/// (the pacing axis). A task never attempted this session is due immediately
/// (subject only to the cap); one already at [`AUTO_RETRY_MAX_ATTEMPTS`] is left
/// for [`UploadEngine::settle_retry_failure`] to move to `GaveUp`, and is never
/// relaunched here — but note it is also excluded upstream because a `GaveUp`
/// row no longer matches `get_failed_retryable`'s `state = 'FAILED'`.
fn auto_retry_due(
    retry_count: u32,
    last_attempt: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    if retry_count >= AUTO_RETRY_MAX_ATTEMPTS {
        return false;
    }
    match last_attempt {
        None => true,
        Some(last) => should_auto_retry(retry_count, now.duration_since(last)),
    }
}

/// Loop-local network-probe debounce state for [`UploadEngine::spawn_auto_retry`].
/// `consecutive_failures` counts probe failures back-to-back (drives `Offline`);
/// `last_emitted` is the last tier pushed to the UI (drives the debounce so
/// [`UploadEvent::NetworkQuality`] fires only on a tier change).
#[derive(Default)]
struct ProbeState {
    consecutive_failures: u32,
    last_emitted: Option<NetworkHealth>,
}

/// A HEAD to `storage.googleapis.com` succeeding slower than this is classified
/// [`NetworkHealth::Weak`]; below it (but at/above [`HEALTH_GOOD_MS`]) is `Ok`.
const HEALTH_WEAK_MS: u128 = 1200;
/// A probe faster than this is [`NetworkHealth::Good`].
const HEALTH_GOOD_MS: u128 = 300;
/// Consecutive failed probes that flip the tier to [`NetworkHealth::Offline`].
const HEALTH_OFFLINE_STREAK: u32 = 2;

/// Time the (proxy-aware) liveness probe and classify a [`NetworkReading`],
/// updating `state.consecutive_failures`. A success below [`HEALTH_GOOD_MS`] is
/// `Good`, up to [`HEALTH_WEAK_MS`] is `Ok`, and slower is `Weak`. A failure is
/// `Weak` until it has failed [`HEALTH_OFFLINE_STREAK`] times in a row, then
/// `Offline` — so a single flaky probe reads as "weak", not "offline".
async fn probe_network(client: &reqwest::Client, state: &mut ProbeState) -> NetworkReading {
    let started = std::time::Instant::now();
    let ok = client
        .head("https://storage.googleapis.com")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .is_ok();
    if !ok {
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let health = if state.consecutive_failures >= HEALTH_OFFLINE_STREAK {
            NetworkHealth::Offline
        } else {
            NetworkHealth::Weak
        };
        return NetworkReading {
            health,
            rtt_ms: None,
        };
    }
    state.consecutive_failures = 0;
    let rtt = started.elapsed().as_millis();
    let health = classify_rtt(rtt);
    NetworkReading {
        health,
        rtt_ms: Some(rtt.min(u32::MAX as u128) as u32),
    }
}

/// Map a successful-probe round-trip (ms) to a health tier. Failure tiers
/// (`Offline`) are decided by the failure streak in [`probe_network`], not here.
fn classify_rtt(rtt_ms: u128) -> NetworkHealth {
    if rtt_ms < HEALTH_GOOD_MS {
        NetworkHealth::Good
    } else if rtt_ms < HEALTH_WEAK_MS {
        NetworkHealth::Ok
    } else {
        NetworkHealth::Weak
    }
}

/// Compose the terminal message for a give-up. On a weak/offline link AND a
/// transport-transient error (the same markers `get_failed_retryable` allow-lists
/// — Network / timeout / no healthy upstream / Interrupted / error sending
/// request), give actionable network copy; otherwise a generic "stopped after N
/// tries" line. English, to match the rest of the desktop UI (the app has no
/// i18n and every other user-facing string is English).
fn compose_give_up_message(attempt: u32, err: &UploadError, reading: NetworkReading) -> String {
    let weak = matches!(reading.health, NetworkHealth::Weak | NetworkHealth::Offline);
    if weak && is_transport_transient(err) {
        return "Network too weak — couldn't reach storage after several retries. Switch to a more stable network, or set a proxy in Settings → Network.".to_string();
    }
    format!("Stopped auto-retrying after {attempt} attempts — retry manually.")
}

/// Whether an error message carries one of the transient/transport markers the
/// auto-retry allow-list keys on (mirrors the `error_message LIKE` clauses in
/// [`crate::db::Database::get_failed_retryable`]). Used only to decide whether a
/// give-up shows network-specific copy — the retry gate itself is the DB query.
fn is_transport_transient(err: &UploadError) -> bool {
    let msg = err.to_string();
    const MARKERS: [&str; 5] = [
        "Network",
        "timeout",
        "no healthy upstream",
        "Interrupted",
        "error sending request",
    ];
    MARKERS.iter().any(|m| msg.contains(m))
}

#[cfg(test)]
mod collect_videos_tests {
    use super::collect_videos_in_dir;
    use std::fs;
    use std::path::PathBuf;

    /// Build a unique temp directory tree for one test and return its root.
    /// Caller removes it via [`cleanup`].
    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lw-collect-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn touch(path: &PathBuf) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::File::create(path).expect("touch file");
    }

    fn cleanup(root: &PathBuf) {
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recurses_nested_dirs_and_filters_non_videos() {
        let root = temp_root("nested");
        touch(&root.join("a.mp4"));
        touch(&root.join("notes.txt"));
        touch(&root.join("sub/b.mov"));
        touch(&root.join("sub/deeper/c.mkv"));
        touch(&root.join("sub/readme.md"));

        let mut found: Vec<String> = collect_videos_in_dir(&root)
            .into_iter()
            .map(|p| {
                p.file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        found.sort();
        cleanup(&root);

        assert_eq!(found, vec!["a.mp4", "b.mov", "c.mkv"]);
    }

    #[test]
    fn empty_dir_yields_no_videos() {
        let root = temp_root("empty");
        let found = collect_videos_in_dir(&root);
        cleanup(&root);
        assert!(found.is_empty());
    }

    #[test]
    fn non_directory_path_yields_no_videos() {
        let root = temp_root("plainfile");
        let file = root.join("clip.mp4");
        touch(&file);
        // Pointing the walk at a file (not a dir) returns empty — plain files
        // go through the per-file staging path, not this recursion.
        let found = collect_videos_in_dir(&file);
        cleanup(&root);
        assert!(found.is_empty());
    }

    #[test]
    fn auto_retry_backoff_grows_exponentially_then_caps() {
        use std::time::Duration;
        // First auto-retry waits one tick; each subsequent one doubles so a
        // file that keeps failing backs off instead of being re-queued every
        // 30s (the storm: one flaky network window re-tried 13 files forever).
        assert_eq!(super::auto_retry_backoff(0), Duration::from_secs(30));
        assert_eq!(super::auto_retry_backoff(1), Duration::from_secs(60));
        assert_eq!(super::auto_retry_backoff(2), Duration::from_secs(120));
        assert_eq!(super::auto_retry_backoff(3), Duration::from_secs(240));
        // Plateaus at 15 minutes so a long-dead file is checked rarely, not never.
        assert_eq!(super::auto_retry_backoff(20), Duration::from_secs(900));
    }

    #[test]
    fn should_auto_retry_waits_for_the_per_task_backoff() {
        use std::time::Duration;
        // retry_count=2 needs 120s since the last attempt before retrying again.
        assert!(!super::should_auto_retry(2, Duration::from_secs(60)));
        assert!(super::should_auto_retry(2, Duration::from_secs(120)));
        // A fresh failure (retry_count=0) is eligible after the first 30s tick.
        assert!(!super::should_auto_retry(0, Duration::from_secs(10)));
        assert!(super::should_auto_retry(0, Duration::from_secs(30)));
    }

    #[test]
    fn auto_retry_due_respects_cap_and_pacing() {
        use super::{AUTO_RETRY_MAX_ATTEMPTS, auto_retry_due};
        use std::time::{Duration, Instant};
        // Advance the `now` argument forward from a fixed base rather than
        // subtracting from `Instant::now()`: `Instant - Duration` panics with
        // "overflow when subtracting duration from instant" on a platform whose
        // monotonic clock starts near boot (e.g. a freshly-booted Windows CI
        // runner). Adding to an Instant is always safe here.
        let base = Instant::now();
        // Never attempted this session: due immediately (below the cap).
        assert!(auto_retry_due(0, None, base));
        // At the cap: never due (would move to GaveUp instead) — this is the
        // give-up axis the persisted retry_count now drives.
        assert!(!auto_retry_due(AUTO_RETRY_MAX_ATTEMPTS, None, base));
        assert!(!auto_retry_due(
            AUTO_RETRY_MAX_ATTEMPTS,
            Some(base),
            base + Duration::from_secs(3600)
        ));
        // Below the cap but too soon since the last attempt: not yet due
        // (retry_count=1 needs 60s of backoff).
        assert!(!auto_retry_due(
            1,
            Some(base),
            base + Duration::from_secs(30)
        ));
        assert!(auto_retry_due(
            1,
            Some(base),
            base + Duration::from_secs(60)
        ));
    }

    #[test]
    fn classify_rtt_maps_bands() {
        use super::{NetworkHealth, classify_rtt};
        assert_eq!(classify_rtt(50), NetworkHealth::Good);
        assert_eq!(classify_rtt(299), NetworkHealth::Good);
        assert_eq!(classify_rtt(300), NetworkHealth::Ok);
        assert_eq!(classify_rtt(1199), NetworkHealth::Ok);
        assert_eq!(classify_rtt(1200), NetworkHealth::Weak);
        assert_eq!(classify_rtt(5000), NetworkHealth::Weak);
    }

    #[test]
    fn give_up_message_is_actionable_on_weak_transport_failure() {
        use super::{NetworkHealth, NetworkReading, compose_give_up_message};
        use crate::error::UploadError;
        let weak = NetworkReading {
            health: NetworkHealth::Weak,
            rtt_ms: Some(3000),
        };
        let good = NetworkReading {
            health: NetworkHealth::Good,
            rtt_ms: Some(50),
        };
        let transient = UploadError::Api {
            status: 503,
            message: "error sending request".to_string(),
        };
        let permanent = UploadError::FileTooLarge { size: 1, max: 0 };
        // Weak link + transport-transient error → network-specific copy.
        assert!(compose_give_up_message(10, &transient, weak).contains("Network too weak"));
        // Healthy link (even with a transient error) → generic copy with count.
        assert!(compose_give_up_message(10, &transient, good).contains("10 attempts"));
        // Weak link but a permanent error → generic copy, not network copy.
        assert!(compose_give_up_message(10, &permanent, weak).contains("10 attempts"));
    }
}
