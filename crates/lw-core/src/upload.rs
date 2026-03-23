use crate::api_client::ApiClient;
use crate::db::Database;
use crate::dedup;
use crate::desensitize;
use crate::error::{DbError, UploadError};
use crate::models::{CreateDocumentMeta, CreateDocumentRequest, UploadState, UploadTask};
use crate::storage::{self, StorageBackend};
use crate::video;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
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
    auto_clean: bool,
    strip_metadata: bool,
    chunk_size: u64,
}

impl UploadEngine {
    pub fn new(
        db: Arc<Database>,
        api: Arc<ApiClient>,
        storage: Arc<StorageBackend>,
        event_tx: mpsc::UnboundedSender<UploadEvent>,
        auto_clean: bool,
        strip_metadata: bool,
        chunk_size_mb: u32,
    ) -> Self {
        Self {
            db,
            api,
            storage,
            event_tx,
            auto_clean,
            strip_metadata,
            chunk_size: (chunk_size_mb as u64) * 1024 * 1024,
        }
    }

    /// Queue a file for upload
    pub async fn queue_file(
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
            state: UploadState::Pending,
            error_message: None,
            hash: None,
            validation_warnings: Vec::new(),
            retry_count: 0,
        };

        self.db.insert_upload_task(&task).await?;
        let _ = self.event_tx.send(UploadEvent::TaskAdded(Box::new(task.clone())));

        Ok(task)
    }

    /// Process a single upload task through all stages
    pub async fn process_task(&self, task: &mut UploadTask) -> Result<(), UploadError> {
        let path_buf = std::path::PathBuf::from(&task.local_path);
        let path = path_buf.as_path();

        // Stage 1: Dedup check
        let hash = dedup::hash_file(path).await?;
        task.hash = Some(hash.clone());

        if let Ok(Some(existing_id)) = self.db.find_by_hash(&hash).await {
            let _ = self.event_tx.send(UploadEvent::DuplicateDetected {
                task_id: task.id.clone(),
                existing_document_id: existing_id.clone(),
            });
            return Err(UploadError::Duplicate { existing_id });
        }

        // Stage 2: Video validation (if video file)
        if task.mime_type.starts_with("video/") {
            self.update_state(task, UploadState::Validating).await;
            match video::validate_video(path).await {
                Ok(result) if !result.warnings.is_empty() => {
                    task.validation_warnings = result.warnings.clone();
                    let _ = self.event_tx.send(UploadEvent::ValidationWarnings {
                        task_id: task.id.clone(),
                        warnings: result.warnings,
                    });
                }
                Err(e) => {
                    tracing::warn!("Video validation failed for {}: {e}", task.filename);
                }
                _ => {}
            }
        }

        // Stage 3: Data desensitization (strip metadata before cross-border upload)
        let mut desensitized_path: Option<PathBuf> = None;
        if self.strip_metadata {
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
                    // Continue with original file
                }
                None => {
                    // File type doesn't need desensitization
                }
            }
        }

        // Use desensitized file for upload if available
        let upload_path = desensitized_path.as_deref().unwrap_or(path);
        let upload_size = tokio::fs::metadata(upload_path).await?.len();

        // Stage 4: Create document on backend
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

        // Stage 4: Get signed upload URL + initiate resumable session
        let signed_url = self
            .api
            .get_upload_url(&task.tenant_id, &task.project_id, &doc.id)
            .await?;

        let session = self
            .storage
            .initiate_upload(&signed_url.url, &task.mime_type, upload_size)
            .await?;

        task.session_id = Some(session.session_id.clone());
        self.db
            .update_upload_session_id(&task.id, &session.session_id)
            .await?;

        // Stage 5: Chunked resumable upload
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
            &session,
            upload_path,
            task.bytes_uploaded,
            self.chunk_size,
            &on_progress,
        )
        .await?;

        task.bytes_uploaded = confirmed;
        let _ = self.db.update_upload_progress(&task.id, confirmed).await;

        // Stage 6: Verify
        self.update_state(task, UploadState::Verifying).await;
        self.api
            .verify_upload(&task.tenant_id, &task.project_id, &doc.id, 10)
            .await?;

        // Stage 7: Complete
        self.update_state(task, UploadState::Completed).await;
        let _ = self.event_tx.send(UploadEvent::Completed {
            task_id: task.id.clone(),
        });

        // Record hash for future dedup
        let _ = self
            .db
            .insert_file_hash(
                &hash,
                &task.filename,
                task.size,
                &task.tenant_id,
                &task.project_id,
                &doc.id,
            )
            .await;

        // Clean up desensitized temp file
        if let Some(ref dp) = desensitized_path {
            desensitize::cleanup_temp_file(dp);
        }

        // Auto-clean original file
        if self.auto_clean && let Err(e) = tokio::fs::remove_file(path).await {
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

        Ok(())
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
