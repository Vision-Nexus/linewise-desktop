use crate::api_client::ApiClient;
use crate::db::Database;
use crate::dedup;
use crate::error::{DbError, UploadError};
use crate::models::{
    CreateDocumentMetadata, CreateDocumentRequest, UploadState, UploadTask,
};
use crate::video;
use std::path::Path;
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
    event_tx: mpsc::UnboundedSender<UploadEvent>,
    auto_clean: bool,
}

impl UploadEngine {
    pub fn new(
        db: Arc<Database>,
        api: Arc<ApiClient>,
        event_tx: mpsc::UnboundedSender<UploadEvent>,
        auto_clean: bool,
    ) -> Self {
        Self {
            db,
            api,
            event_tx,
            auto_clean,
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
            gcs_session_uri: None,
            bytes_uploaded: 0,
            state: UploadState::Pending,
            error_message: None,
            hash: None,
            validation_warnings: Vec::new(),
            retry_count: 0,
        };

        self.db.insert_upload_task(&task)?;
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

        if let Ok(Some(existing_id)) = self.db.find_by_hash(&hash) {
            let _ = self.event_tx.send(UploadEvent::DuplicateDetected {
                task_id: task.id.clone(),
                existing_document_id: existing_id.clone(),
            });
            return Err(UploadError::Duplicate {
                existing_id,
            });
        }

        // Stage 2: Video validation (if video file)
        if task.mime_type.starts_with("video/") {
            self.update_state(task, UploadState::Validating);
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

        // Stage 3: Create document on backend
        self.update_state(task, UploadState::Creating);
        let doc = self
            .api
            .create_document(
                &task.tenant_id,
                &task.project_id,
                &CreateDocumentRequest {
                    collection: "documents".to_string(),
                    description: task.filename.clone(),
                    metadata: CreateDocumentMetadata {
                        filename: task.filename.clone(),
                        size: task.size,
                        mime_type: Some(task.mime_type.clone()),
                    },
                },
            )
            .await?;

        task.document_id = Some(doc.id.clone());
        self.db.update_upload_document_id(&task.id, &doc.id)?;

        // Stage 4: Get signed upload URL
        let signed_url = self
            .api
            .get_upload_url(&task.tenant_id, &task.project_id, &doc.id)
            .await?;

        // Stage 5: Upload file
        self.update_state(task, UploadState::Uploading);
        let data = tokio::fs::read(path).await?;
        let total = data.len() as u64;

        self.api
            .upload_to_signed_url(&signed_url.url, data, &task.mime_type)
            .await?;

        task.bytes_uploaded = total;
        let _ = self.db.update_upload_progress(&task.id, total);
        let _ = self.event_tx.send(UploadEvent::Progress {
            task_id: task.id.clone(),
            bytes_uploaded: total,
            total_bytes: total,
        });

        // Stage 6: Verify
        self.update_state(task, UploadState::Verifying);
        self.api
            .verify_upload(&task.tenant_id, &task.project_id, &doc.id, 10)
            .await?;

        // Stage 7: Complete
        self.update_state(task, UploadState::Completed);
        let _ = self.event_tx.send(UploadEvent::Completed {
            task_id: task.id.clone(),
        });

        // Record hash for future dedup
        let _ = self.db.insert_file_hash(
            &hash,
            &task.filename,
            task.size,
            &task.tenant_id,
            &task.project_id,
            &doc.id,
        );

        // Auto-clean
        if self.auto_clean && let Err(e) = tokio::fs::remove_file(path).await {
            tracing::warn!("Failed to auto-clean {}: {e}", task.local_path);
        }

        Ok(())
    }

    /// Resume pending uploads from database
    pub async fn resume_pending(&self) -> Result<(), DbError> {
        let tasks = self.db.get_pending_uploads()?;
        tracing::info!("Resuming {} pending uploads", tasks.len());

        for mut task in tasks {
            let engine = UploadEngine {
                db: Arc::clone(&self.db),
                api: Arc::clone(&self.api),
                event_tx: self.event_tx.clone(),
                auto_clean: self.auto_clean,
            };

            tokio::spawn(async move {
                match engine.process_task(&mut task).await {
                    Ok(()) => tracing::info!("Upload completed: {}", task.filename),
                    Err(e) => {
                        tracing::error!("Upload failed for {}: {e}", task.filename);
                        let _ = engine.db.update_upload_state(
                            &task.id,
                            UploadState::Failed,
                            Some(&e.to_string()),
                        );
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

    fn update_state(&self, task: &mut UploadTask, state: UploadState) {
        let _ = self.event_tx.send(UploadEvent::StateChanged {
            task_id: task.id.clone(),
            state: state.clone(),
        });
        let _ = self.db.update_upload_state(&task.id, state.clone(), None);
        task.state = state;
    }
}
