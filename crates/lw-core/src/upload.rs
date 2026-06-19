use crate::api_client::ApiClient;
use crate::config::TranscodeConfig;
use crate::container_kind::{self, ContainerKind};
use crate::db::Database;
use crate::dedup;
use crate::desensitize;
use crate::error::{DbError, UploadError, VideoValidationError};
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

/// Events emitted by the upload engine to the UI
#[derive(Debug, Clone)]
pub enum UploadEvent {
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
}

/// Cap on how many files run their staging-time quality check + hash stream
/// concurrently. Staging 100 files at once otherwise spawns 100 background
/// workers that each open a server `/quality-check` round-trip and a streaming
/// BLAKE3+MD5 read — flooding the event channel and saturating the network.
/// Throttling the QC/hash churn to a trickle keeps the row inserts (and their
/// `TaskAdded` events) instant while the heavy work drains a few at a time.
/// Uploads themselves are bounded separately by `upload_semaphore`.
const MAX_CONCURRENT_STAGING: usize = 4;

pub struct UploadEngine {
    db: Arc<Database>,
    api: Arc<ApiClient>,
    storage: Arc<StorageBackend>,
    event_tx: mpsc::UnboundedSender<UploadEvent>,
    /// Whether to delete the original file on disk after a successful upload.
    /// Flipped live from the settings UI; reads are `Relaxed` because each
    /// upload task reads this exactly once, after the upload has already
    /// completed, so ordering against other work is irrelevant.
    auto_clean: AtomicBool,
    strip_metadata: bool,
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
        strip_metadata: bool,
        transcode_config: TranscodeConfig,
        chunk_size_mb: u32,
        max_concurrent: u32,
    ) -> Self {
        Self {
            db,
            api,
            storage,
            event_tx,
            auto_clean: AtomicBool::new(auto_clean),
            strip_metadata,
            transcode_config,
            chunk_size: (chunk_size_mb as u64) * 1024 * 1024,
            upload_semaphore: Arc::new(Semaphore::new(max_concurrent as usize)),
            stage_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_STAGING)),
            current_user_cache: OnceCell::new(),
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
    /// Three-phase: this function returns synchronously after
    /// inserting the row in the `QualityChecking` state, and spawns a
    /// background worker that (1) runs the local atom walk + server
    /// `/quality-check` round-trip, (2) flips the row to `Hashing`
    /// and streams BLAKE3+MD5, (3) runs the cross-tenant dedup check
    /// and settles the row in `Staged` or `Rejected`. Each phase
    /// emits its own `StateChanged` so the queue UI can render an
    /// indeterminate progress bar during the network-bound check and
    /// a determinate one during hashing.
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
    /// Quality-`Rejected` rows still go through hashing: a
    /// super-admin "Force upload" bypass turns them into a normal
    /// pipeline run, and that run wants the digest already on the row
    /// so the `create_document` call can carry `digest.{md5, crc32c,
    /// sha256_head_256kib}` without paying for a second I/O pass.
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
            // server gate runs only for `video/*`. They land in
            // `Hashing` straight away and follow the regular dedup
            // path. Video rows enter `QualityChecking` and only
            // transition to `Hashing` once the server response arrives.
            state: if is_video {
                UploadState::QualityChecking
            } else {
                UploadState::Hashing
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
        };

        self.db.insert_upload_task(&task).await?;
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
                    .run_quality_then_hash(
                        &task_id,
                        &path_buf,
                        &tenant_id_owned,
                        &project_id_owned,
                        &filename,
                    )
                    .await
            } else {
                // Non-video rows have no `video_info`, so the transcode-hold
                // predicate in `run_hash_and_dedup` can never fire for them —
                // they always auto-advance on a `Staged` settle.
                engine
                    .run_hash_and_dedup(&task_id, &path_buf, &tenant_id_owned, Vec::new(), None)
                    .await
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

    /// Drive the quality-check phase, then hand off to the hash phase.
    /// On a broken-file / unsupported-container / offline failure the
    /// row settles directly into `Rejected` with the typed message in
    /// `rejection_reasons`; on accept (or server-reject) the row
    /// transitions to `Hashing` and the regular post-hash path runs.
    async fn run_quality_then_hash(
        self: &Arc<Self>,
        task_id: &str,
        path: &Path,
        tenant_id: &str,
        project_id: &str,
        filename: &str,
    ) -> UploadState {
        match self
            .run_quality_check(path, tenant_id, project_id, filename)
            .await
        {
            Ok((video_info, warnings, post_hash)) => {
                // Persist the response payload + flip to Hashing in
                // one write so the popover-data fields land atomically
                // with the state change.
                let _ = self
                    .db
                    .update_upload_quality_check_settled(
                        task_id,
                        UploadState::Hashing,
                        video_info.as_ref(),
                        &warnings,
                    )
                    .await;
                // Keep a copy for the transcode-hold decision at the `Staged`
                // settle point; the event takes ownership of the original.
                let video_info_for_hold = video_info.clone();
                let _ = self.event_tx.send(UploadEvent::QualityCheckPassed {
                    task_id: task_id.to_string(),
                    video_info,
                    warnings: warnings.clone(),
                });
                let _ = self.event_tx.send(UploadEvent::StateChanged {
                    task_id: task_id.to_string(),
                    state: UploadState::Hashing,
                });
                match post_hash {
                    PostHashVerdict::Stage => {
                        self.run_hash_and_dedup(
                            task_id,
                            path,
                            tenant_id,
                            warnings,
                            video_info_for_hold,
                        )
                        .await
                    }
                    PostHashVerdict::Reject(reasons) => {
                        self.run_hash_only(task_id, path, warnings, reasons).await
                    }
                }
            }
            Err(err) => self.settle_quality_check_rejected(task_id, &err).await,
        }
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

    /// Drive the hash stream for a `Hashing` row, then run the
    /// cross-tenant dedup check, and end in `Staged` or `Rejected`.
    /// Returns the terminal state for logging only — all persistence
    /// and UI events happen inside.
    ///
    /// On a `Staged` settle the row is auto-advanced to `Pending` and
    /// dispatched immediately — there is no manual "Upload" step on the
    /// happy path — UNLESS this clip is held for an opt-in transcode (see
    /// [`Self::held_for_transcode`]), in which case it stays `Staged` and
    /// waits for the manual confirm. `video_info` carries the quality-check
    /// probe so the hold predicate can run; it is `None` for non-video rows
    /// (which are never held).
    async fn run_hash_and_dedup(
        self: &Arc<Self>,
        task_id: &str,
        path: &Path,
        tenant_id: &str,
        warnings: Vec<String>,
        video_info: Option<crate::models::VideoInfo>,
    ) -> UploadState {
        let hashes = match self.consume_hash_stream(task_id, path).await {
            Ok(h) => h,
            Err(_) => return self.fail_hashing(task_id).await,
        };
        // Tier-2 perceptual frames (empty + no-op while `pdq::PDQ_ENABLED` is
        // false). Computed here so the dedup query can carry them; recomputed at
        // Stage 4 for persistence (see `process_task`).
        let pdq_frames = pdq::compute_pdq_frames(path).await;

        let mut rejection_reasons: Vec<String> = Vec::new();
        let final_state = match self
            .dedup_verdict(tenant_id, &hashes, pdq_frames, task_id)
            .await
        {
            DedupVerdict::Allow => UploadState::Staged,
            DedupVerdict::Reuse(document_id) => {
                // Stamp the abandoned upload's document_id onto the row
                // so Stage 4 in `process_task` skips `create_document`
                // and the resumable-upload machinery picks the existing
                // GCS object. A failure here drops us back to the
                // regular staging path — the worst case is the row gets
                // a fresh document on retry, which is the pre-feature
                // behaviour, so we don't escalate to Rejected.
                if let Err(e) = self
                    .db
                    .update_upload_document_id(task_id, &document_id)
                    .await
                {
                    tracing::warn!(
                        task_id,
                        document_id,
                        "dedup: failed to persist reused document_id, falling back to fresh create: {e}"
                    );
                }
                UploadState::Staged
            }
            DedupVerdict::Reject(reason) => {
                rejection_reasons.push(reason);
                UploadState::Rejected
            }
        };

        let settled = self
            .settle_post_hash(task_id, final_state, warnings, rejection_reasons)
            .await;

        // Auto-upload: a row that settled to `Staged` advances straight to
        // `Pending` and dispatches, removing the manual click between QC-pass
        // and upload. Transcode-eligible clips are the only exception — they
        // wait `Staged` for the opt-in transcode confirm.
        match settled {
            UploadState::Staged if !self.held_for_transcode(video_info.as_ref()) => {
                self.advance_staged_and_dispatch(task_id).await;
            }
            UploadState::QualityChecking
            | UploadState::Hashing
            | UploadState::Staged
            | UploadState::Rejected
            | UploadState::Pending
            | UploadState::Validating
            | UploadState::Transcoding
            | UploadState::Desensitizing
            | UploadState::Creating
            | UploadState::Uploading
            | UploadState::Verifying
            | UploadState::Completed
            | UploadState::Failed
            | UploadState::Paused => {}
        }
        settled
    }

    /// Whether auto-upload should HOLD this clip `Staged` for the manual
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
    /// the `confirm_staged` loop does for a single task, so auto-upload and
    /// the manual confirm share one dispatch path. Flipping to `Pending`
    /// before the load means the manual `[Upload]` button (which reads
    /// STAGED rows) can never also grab this row.
    async fn advance_staged_and_dispatch(self: &Arc<Self>, task_id: &str) {
        if let Err(e) = self
            .db
            .update_upload_state(task_id, UploadState::Pending, None)
            .await
        {
            tracing::warn!(task_id, "auto-upload: failed to mark row PENDING: {e}");
            return;
        }
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task_id.to_string(),
            state: UploadState::Pending,
        });

        // O(1) single-row load now that the row is `Pending` in the DB —
        // avoids loading and scanning the entire pending set once per
        // auto-advancing task.
        let task = match self.db.get_upload_by_id(task_id).await {
            Ok(Some(task)) => task,
            Ok(None) => {
                tracing::warn!(task_id, "auto-upload: task not found after flip to PENDING");
                return;
            }
            Err(e) => {
                tracing::warn!(task_id, "auto-upload: get_upload_by_id failed: {e}");
                return;
            }
        };
        self.dispatch_one(task);
    }

    /// Spawn the bounded-parallel upload worker for one already-`Pending`
    /// task. The single per-task dispatch path: a permit from the
    /// `upload_semaphore` caps concurrency at `max_concurrent`, and a failure
    /// settles the row to `Failed` with the typed error. Shared by
    /// `confirm_staged` (manual confirm) and `advance_staged_and_dispatch`
    /// (auto-upload) so both fan out identically. Per-task (not per-batch) so
    /// a slow QC file never gates a fast one.
    fn dispatch_one(self: &Arc<Self>, mut task: UploadTask) {
        let engine = Arc::clone(self);
        let sem = Arc::clone(&self.upload_semaphore);
        tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            match engine.process_task(&mut task).await {
                Ok(()) => tracing::info!("Upload completed: {}", task.filename),
                Err(e) => {
                    e.log(format_args!("Upload of {}", task.filename));
                    let _ = engine
                        .db
                        .update_upload_state(&task.id, UploadState::Failed, Some(&e.to_string()))
                        .await;
                    let _ = engine.event_tx.send(UploadEvent::Failed {
                        task_id: task.id,
                        error: e.to_string(),
                    });
                }
            }
        });
    }

    /// Hash a quality-rejected row so the MD5 is on disk for a future
    /// Force-upload bypass, then settle the row into `Rejected` with
    /// the quality reasons. We skip the dedup check here — a row the
    /// user can't upload as-is doesn't need the cross-tenant verdict.
    async fn run_hash_only(
        self: &Arc<Self>,
        task_id: &str,
        path: &Path,
        warnings: Vec<String>,
        rejection_reasons: Vec<String>,
    ) -> UploadState {
        if self.consume_hash_stream(task_id, path).await.is_err() {
            return self.fail_hashing(task_id).await;
        }
        self.settle_post_hash(task_id, UploadState::Rejected, warnings, rejection_reasons)
            .await
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

    /// Pump the hash stream into UI events + persisted hashes. The
    /// single drain path used both by the staging-time worker and by
    /// `process_task`'s legacy-row fallback. Errors are surfaced as
    /// `UploadError::Io` so a caller in the upload pipeline can `?`
    /// them; the staging worker maps `Err` to a `Failed` state via
    /// [`fail_hashing`]. `HashProgress` events fire unconditionally —
    /// a row that isn't in the `Hashing` state has its stale entry
    /// cleared by the next `StateChanged` in the UI's event handler.
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

    /// Mark a row Failed when its hash stream errored. Surfaced to
    /// the UI so the user can re-add the file rather than wonder
    /// why the row is stuck on "Hashing" forever.
    async fn fail_hashing(self: &Arc<Self>, task_id: &str) -> UploadState {
        let msg = "Failed to read file for hashing";
        let _ = self
            .db
            .update_upload_state(task_id, UploadState::Failed, Some(msg))
            .await;
        let _ = self.event_tx.send(UploadEvent::Failed {
            task_id: task_id.to_string(),
            error: msg.to_string(),
        });
        UploadState::Failed
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
        let plural = if tenant_match_count == 1 { "" } else { "s" };
        DedupVerdict::Reject(format!(
            "Already uploaded in this tenant ({tenant_match_count} document{plural})",
        ))
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

    /// Confirm staged files for upload (step 2 of two-step upload).
    /// Moves all STAGED tasks to PENDING and dispatches each through the
    /// bounded-parallel worker. With auto-upload (PR3) this only ever acts on
    /// clips HELD `Staged` for an opt-in transcode — everything else already
    /// auto-advanced at QC-pass time.
    pub async fn confirm_staged(
        self: &Arc<Self>,
        transcode_task_ids: &[String],
    ) -> Result<Vec<String>, DbError> {
        let staged = self.db.get_staged_uploads().await?;
        let mut confirmed_ids = Vec::new();

        for mut task in staged {
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

    /// Process a single upload task through all stages.
    /// Resumes from where it left off — skips stages already completed
    /// (has document_id → skip create, has session_id → skip initiate).
    pub async fn process_task(&self, task: &mut UploadTask) -> Result<(), UploadError> {
        let path_buf = std::path::PathBuf::from(&task.local_path);
        let path = path_buf.as_path();

        // Stage 1: Dedup check (skip if already hashed). Staging now
        // pre-computes the full 4-way hash (BLAKE3 + MD5 + CRC32C +
        // SHA-256-head), so on the happy path we already have all four
        // values on the row. This branch fires for two cases:
        //
        //   1. Legacy rows staged before the dual-hash pass landed
        //      (`task.hash` is None).
        //   2. Rows staged in the BLAKE3+MD5-only era after the
        //      multi-signal-digest migration: `task.hash` is set but
        //      `source_crc32c` / `source_sha256_head_256kib` are NULL.
        //      Without rehashing, Stage 4's `create_document` would
        //      send a partial `digest` and the GCS-callback verified
        //      pair couldn't match the desktop-supplied legs.
        //
        // The `force_upload` flag suppresses both the local-DB
        // short-circuit and (implicitly) the staging-time dedup gate
        // that would have already routed this row to `Rejected` —
        // a force-upload row reaches `process_task` only because a
        // super-admin clicked the bypass, so re-asserting the gate
        // here would defeat the affordance.
        let needs_rehash = task.hash.is_none()
            || task.source_crc32c.is_none()
            || task.source_sha256_head_256kib.is_none();
        if needs_rehash {
            let hashes = self.consume_hash_stream(&task.id, path).await?;
            task.hash = Some(hashes.blake3_hex.clone());
            task.source_md5 = Some(hashes.md5_hex.clone());
            task.source_crc32c = Some(hashes.crc32c_b64.clone());
            task.source_sha256_head_256kib = Some(hashes.sha256_head_256kib_hex.clone());

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
        // Set unconditionally above: either the row arrived from `Hashing`
        // with `task.hash` populated by `consume_hash_stream`, or the
        // legacy fallback in this function just wrote it. An empty hash
        // here would silently match the wrong row in `find_by_hash` /
        // `insert_file_hash`, so fail loudly instead.
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

        // Stage 2.5: Transcoding (user opt-in, video files only)
        let transcoded_path = self.maybe_transcode(task, path, &video_info).await?;

        // Stage 3: Data desensitization (skip if already transcoded — transcoding strips metadata)
        let mut desensitized_path: Option<PathBuf> = None;
        if self.strip_metadata && transcoded_path.is_none() {
            self.update_state(task, UploadState::Desensitizing).await;
            match desensitize::strip_metadata(path, &task.mime_type).await {
                Some(Ok(result)) => {
                    tracing::info!(
                        "Metadata stripped from {}: output at {}",
                        task.filename,
                        result.output_path.display()
                    );
                    desensitized_path = Some(result.output_path);
                }
                Some(Err(e)) => {
                    tracing::warn!("Desensitization failed for {}: {e}", task.filename);
                }
                None => {}
            }
        }

        let upload_path = transcoded_path
            .as_deref()
            .or(desensitized_path.as_deref())
            .unwrap_or(path);
        let upload_size = tokio::fs::metadata(upload_path).await?.len();

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
            // Recompute the perceptual frames (cheap: <=5 keyframe seeks) so the
            // created doc is persisted as a near-duplicate match target. Empty +
            // omitted while `pdq::PDQ_ENABLED` is false. Recomputed rather than
            // carried from staging to avoid a DB column / migration — the source
            // file is local and unchanged, so the frames are identical.
            let pdq_frames = pdq::compute_pdq_frames(path).await;

            // INVARIANT: every `create_document` is immediately preceded by a
            // fresh dedup check. This is the one choke point that runs on *every*
            // path to create — happy path, auto-retry, and resume — unlike the
            // staging check (which ran once, before this row's own document
            // existed) and the Stage-1 checks (which only fire for rows that need
            // rehashing). It adopts the in-flight orphan a lost create-response
            // left behind (instead of minting a second document) and skips
            // creating when a duplicate has appeared since staging.
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
                        &desensitized_path,
                        &transcoded_path,
                    )
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
                        &desensitized_path,
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
                            &desensitized_path,
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
            &desensitized_path,
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
        desensitized_path: &Option<PathBuf>,
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

        let confirmed = storage::upload_file_chunked(
            self.storage.as_ref(),
            session,
            upload_path,
            task.bytes_uploaded,
            chunk_size,
            max_retries,
            &on_progress,
        )
        .await?;

        task.bytes_uploaded = confirmed;
        let _ = self.db.update_upload_progress(&task.id, confirmed).await;

        self.finalize_after_upload(task, hash, desensitized_path, transcoded_path)
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
        desensitized_path: &Option<PathBuf>,
        transcoded_path: &Option<PathBuf>,
    ) -> Result<(), UploadError> {
        let doc_id = task.document_id.clone().unwrap_or_default();

        // Verify
        self.update_state(task, UploadState::Verifying).await;
        self.api
            .verify_upload(&task.tenant_id, &task.project_id, &doc_id, 10)
            .await?;

        // Complete
        self.update_state(task, UploadState::Completed).await;
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

        // Clean up temp files
        if let Some(dp) = desensitized_path {
            desensitize::cleanup_temp_file(dp);
        }
        if let Some(tp) = transcoded_path {
            desensitize::cleanup_temp_file(tp);
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
        desensitized_path: &Option<PathBuf>,
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
        )
        .await?;

        task.bytes_uploaded = upload_size;
        let _ = self.db.update_upload_progress(&task.id, upload_size).await;

        self.finalize_after_upload(task, hash, desensitized_path, transcoded_path)
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
        desensitized_path: &Option<PathBuf>,
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
        )
        .await?;

        task.bytes_uploaded = upload_size;
        let _ = self.db.update_upload_progress(&task.id, upload_size).await;

        self.finalize_after_upload(task, hash, desensitized_path, transcoded_path)
            .await
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
                    Err(e) => {
                        e.log(format_args!("Upload of {}", task.filename));
                        let _ = engine
                            .db
                            .update_upload_state(
                                &task.id,
                                UploadState::Failed,
                                Some(&e.to_string()),
                            )
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

    /// Spawn a background task that auto-retries failed uploads when network recovers.
    /// Checks every 30 seconds for failed tasks with "Network error" and retries them.
    pub fn spawn_auto_retry(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;

                // Check if network is available
                let online = reqwest::Client::new()
                    .head("https://storage.googleapis.com")
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await
                    .is_ok();

                if !online {
                    continue;
                }

                // Find failed tasks that are retryable (network errors)
                let failed = match engine.db.get_failed_retryable().await {
                    Ok(tasks) => tasks,
                    Err(_) => continue,
                };

                if failed.is_empty() {
                    continue;
                }

                tracing::info!(
                    "Auto-retrying {} failed uploads after network recovery",
                    failed.len()
                );

                for task in failed {
                    let eng = Arc::clone(&engine);
                    let sem = Arc::clone(&engine.upload_semaphore);
                    tokio::spawn(Self::retry_task(eng, sem, task));
                }
            }
        })
    }

    async fn retry_task(eng: Arc<Self>, sem: Arc<Semaphore>, mut task: UploadTask) {
        let _permit = sem.acquire().await.expect("semaphore closed");
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
            Err(e) => {
                tracing::warn!("Auto-retry failed for {}: {e}", task.filename);
                let _ = eng
                    .db
                    .update_upload_state(&task.id, UploadState::Failed, Some(&e.to_string()))
                    .await;
                let _ = eng.event_tx.send(UploadEvent::Failed {
                    task_id: task.id,
                    error: e.to_string(),
                });
            }
        }
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
}
