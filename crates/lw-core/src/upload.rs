use crate::api_client::ApiClient;
use crate::config::TranscodeConfig;
use crate::db::Database;
use crate::dedup;
use crate::desensitize;
use crate::error::{DbError, UploadError};
use crate::models::{CreateDocumentMeta, CreateDocumentRequest, UploadState, UploadTask};
use crate::storage::{self, StorageBackend};
use crate::{transcode, video};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Semaphore, mpsc};
use uuid::Uuid;

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
    ValidationWarnings {
        task_id: String,
        warnings: Vec<String>,
    },
    TranscodeProgress {
        task_id: String,
        percent: f32,
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
        }
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

    /// Stage a file for review (step 1 of two-step upload).
    /// File is added to the list but NOT uploaded yet.
    pub async fn stage_file(
        &self,
        path: &Path,
        tenant_id: &str,
        project_id: &str,
    ) -> Result<UploadTask, UploadError> {
        if !path.exists() {
            return Err(UploadError::FileNotFound(path.to_path_buf()));
        }

        let metadata = std::fs::metadata(path)?;
        let filename = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mime_type = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        // Probe video files at staging time so info is available in the UI
        let (video_info, validation_warnings) = if mime_type.starts_with("video/") {
            match video::validate_video(path).await {
                Ok(result) => (Some(result.info), result.warnings),
                Err(e) => {
                    tracing::warn!("Video probe failed for {filename}: {e}");
                    (None, Vec::new())
                }
            }
        } else {
            (None, Vec::new())
        };

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
            bytes_uploaded: 0,
            state: UploadState::Staged,
            error_message: None,
            hash: None,
            validation_warnings,
            retry_count: 0,
            transcode: false,
            video_info,
        };

        self.db.insert_upload_task(&task).await?;
        let _ = self
            .event_tx
            .send(UploadEvent::TaskAdded(Box::new(task.clone())));

        Ok(task)
    }

    /// Confirm staged files for upload (step 2 of two-step upload).
    /// Moves all STAGED tasks to PENDING and starts processing.
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

            let engine = Arc::clone(self);
            let sem = Arc::clone(&self.upload_semaphore);
            tokio::spawn(async move {
                // Limit concurrent uploads
                let _permit = sem.acquire().await.expect("semaphore closed");
                match engine.process_task(&mut task).await {
                    Ok(()) => tracing::info!("Upload completed: {}", task.filename),
                    Err(e) => {
                        tracing::error!("Upload failed for {}: {e}", task.filename);
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

        // Stage 1: Dedup check (skip if already hashed)
        if task.hash.is_none() {
            let hash = dedup::hash_file(path).await?;
            task.hash = Some(hash.clone());

            if let Ok(Some(existing_id)) = self.db.find_by_hash(&hash).await {
                let _ = self.event_tx.send(UploadEvent::DuplicateDetected {
                    task_id: task.id.clone(),
                    existing_document_id: existing_id.clone(),
                });
                return Err(UploadError::Duplicate { existing_id });
            }
        }
        let hash = task.hash.clone().unwrap_or_default();

        // Stage 2: Video validation (skip if already probed at staging time)
        let video_info = if task.video_info.is_some() {
            task.video_info.clone()
        } else if task.mime_type.starts_with("video/") {
            self.update_state(task, UploadState::Validating).await;
            match video::validate_video(path).await {
                Ok(result) => {
                    if !result.warnings.is_empty() {
                        task.validation_warnings = result.warnings.clone();
                        let _ = self.event_tx.send(UploadEvent::ValidationWarnings {
                            task_id: task.id.clone(),
                            warnings: result.warnings,
                        });
                    }
                    Some(result.info)
                }
                Err(e) => {
                    tracing::warn!("Video validation failed for {}: {e}", task.filename);
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

        // Stage 4: Create document (skip if already has document_id)
        let doc_id = if let Some(ref doc_id) = task.document_id {
            tracing::info!("Resuming: document already created ({})", doc_id);
            doc_id.clone()
        } else {
            self.update_state(task, UploadState::Creating).await;
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
                    },
                )
                .await?;
            task.document_id = Some(doc.id.clone());
            self.db.update_upload_document_id(&task.id, &doc.id).await?;
            doc.id
        };

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
        _upload_size: u64,
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

        let confirmed = storage::upload_file_chunked(
            self.storage.as_ref(),
            session,
            upload_path,
            task.bytes_uploaded,
            self.chunk_size,
            &on_progress,
        )
        .await?;

        task.bytes_uploaded = confirmed;
        let _ = self.db.update_upload_progress(&task.id, confirmed).await;

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

    /// Resume pending uploads from database
    pub async fn resume_pending(self: &Arc<Self>) -> Result<(), DbError> {
        let tasks = self.db.get_pending_uploads().await?;
        tracing::info!("Resuming {} pending uploads", tasks.len());

        for mut task in tasks {
            let engine = Arc::clone(self);

            tokio::spawn(async move {
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
                        tracing::error!("Upload failed for {}: {e}", task.filename);
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
