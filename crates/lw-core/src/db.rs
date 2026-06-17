use crate::config::AppConfig;
use crate::dedup::FileHashes;
use crate::error::DbError;
use crate::models::{UploadState, UploadTask};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

pub struct Database {
    pool: SqlitePool,
}

/// Row type matching SQLite nullability for sqlx::query_as!
/// SQLite treats all columns as potentially nullable, so we use Option everywhere
/// and convert in From impl.
#[derive(Debug)]
struct UploadRow {
    id: Option<String>,
    local_path: Option<String>,
    filename: Option<String>,
    size: Option<i64>,
    mime_type: Option<String>,
    tenant_id: Option<String>,
    project_id: Option<String>,
    document_id: Option<String>,
    session_id: Option<String>,
    bytes_uploaded: Option<i64>,
    state: Option<String>,
    error_message: Option<String>,
    hash: Option<String>,
    source_md5: Option<String>,
    source_crc32c: Option<String>,
    source_sha256_head_256kib: Option<String>,
    validation_warnings: Option<String>,
    rejection_reasons: Option<String>,
    retry_count: Option<i64>,
    video_info: Option<String>,
    transcode: i64,
    transcoded_size: Option<i64>,
    force_upload: i64,
}

impl From<UploadRow> for UploadTask {
    fn from(r: UploadRow) -> Self {
        let warnings: Vec<String> =
            serde_json::from_str(r.validation_warnings.as_deref().unwrap_or("[]"))
                .unwrap_or_default();
        let rejection_reasons: Vec<String> =
            serde_json::from_str(r.rejection_reasons.as_deref().unwrap_or("[]"))
                .unwrap_or_default();
        Self {
            id: r.id.unwrap_or_default(),
            local_path: r.local_path.unwrap_or_default(),
            filename: r.filename.unwrap_or_default(),
            size: r.size.unwrap_or(0) as u64,
            mime_type: r.mime_type.unwrap_or_default(),
            tenant_id: r.tenant_id.unwrap_or_default(),
            project_id: r.project_id.unwrap_or_default(),
            document_id: r.document_id,
            session_id: r.session_id,
            bytes_uploaded: r.bytes_uploaded.unwrap_or(0) as u64,
            state: UploadState::parse(r.state.as_deref().unwrap_or("PENDING")),
            error_message: r.error_message,
            hash: r.hash,
            source_md5: r.source_md5,
            source_crc32c: r.source_crc32c,
            source_sha256_head_256kib: r.source_sha256_head_256kib,
            validation_warnings: warnings,
            rejection_reasons,
            retry_count: r.retry_count.unwrap_or(0) as u32,
            transcode: r.transcode != 0,
            transcoded_size: r.transcoded_size.map(|v| v as u64),
            video_info: r
                .video_info
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
            force_upload: r.force_upload != 0,
        }
    }
}

impl Database {
    /// Delete the on-disk SQLite database so the next `open()` call starts
    /// from a fresh schema. Used as an escape hatch when a migration fails
    /// on a legacy DB that we can't resolve in place. Also removes the
    /// WAL / SHM sidecar files, otherwise SQLite can replay a stale log
    /// against the new DB and re-corrupt it.
    ///
    /// The upload queue is local-only state: every task has a remote
    /// counterpart (either a staged file on disk the user can re-add, or
    /// an already-uploaded document tracked by the API), so resetting is
    /// safe — the user loses queue history but no uploaded data.
    #[tracing::instrument(skip_all)]
    pub fn reset_local_files() -> Result<(), DbError> {
        tracing::warn!("resetting local sqlite files (destructive recovery path)");
        let db_path = AppConfig::db_path();
        for suffix in ["", "-wal", "-shm"] {
            let mut p = db_path.clone();
            let file_name = match p.file_name().and_then(|s| s.to_str()) {
                Some(name) => format!("{name}{suffix}"),
                None => continue,
            };
            p.set_file_name(file_name);
            if p.exists()
                && let Err(e) = std::fs::remove_file(&p)
            {
                return Err(DbError::Migration(format!(
                    "Failed to remove {}: {e}",
                    p.display()
                )));
            }
        }
        Ok(())
    }

    /// Drain in-flight queries and close every connection in the pool.
    ///
    /// Why this exists: `Database` is held inside `Arc<Database>` and
    /// cloned widely (CoreServices, the engine, the auto-retry worker,
    /// the Dioxus context provider). Dropping the outer Arc is not
    /// enough to release SQLite's file/WAL/SHM locks while sibling Arcs
    /// are still alive. `SqlitePool::close` runs SQLite's `xClose` on
    /// every pooled connection regardless of how many `Arc<Database>`
    /// references remain — when it returns, the file handles are gone
    /// and `reset_local_files` can safely unlink the on-disk files.
    ///
    /// Without this step, `wipe_db` raced the still-open pool: SQLite
    /// would re-create WAL/SHM sidecars during the wipe and the user
    /// would be left with a "wiped" DB that immediately came back.
    #[tracing::instrument(skip_all)]
    pub async fn close(&self) {
        self.pool.close().await;
        tracing::warn!("sqlite pool closed");
    }

    #[tracing::instrument(skip_all)]
    pub async fn open() -> Result<Self, DbError> {
        let db_path = AppConfig::db_path();
        std::fs::create_dir_all(db_path.parent().expect("db path must have parent"))
            .map_err(|e| DbError::Migration(e.to_string()))?;

        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))
            .map_err(|e| DbError::Migration(e.to_string()))?
            .journal_mode(SqliteJournalMode::Wal)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .map_err(DbError::Sqlite)?;

        // Run migrations from crates/lw-core/migrations/
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| DbError::Migration(e.to_string()))?;
        tracing::info!("Database migrations applied");

        Ok(Self { pool })
    }

    // -- Upload Queue --

    #[tracing::instrument(skip_all, fields(
        task_id = %task.id,
        tenant = %task.tenant_id,
        filename = %task.filename,
        size = task.size,
    ))]
    pub async fn insert_upload_task(&self, task: &UploadTask) -> Result<(), DbError> {
        let warnings_json = serde_json::to_string(&task.validation_warnings)?;
        let reasons_json = serde_json::to_string(&task.rejection_reasons)?;
        let video_info_json = task
            .video_info
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let size = task.size as i64;
        let state = task.state.as_str();
        let transcode = i64::from(task.transcode);
        let force_upload = i64::from(task.force_upload);
        sqlx::query!(
            "INSERT INTO upload_queue (id, local_path, filename, size, mime_type, tenant_id, project_id, state, hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, video_info, transcode, force_upload)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            task.id,
            task.local_path,
            task.filename,
            size,
            task.mime_type,
            task.tenant_id,
            task.project_id,
            state,
            task.hash,
            task.source_md5,
            task.source_crc32c,
            task.source_sha256_head_256kib,
            warnings_json,
            reasons_json,
            video_info_json,
            transcode,
            force_upload,
        )
        .execute(&self.pool)
        .await?;
        tracing::info!("queued upload task");
        Ok(())
    }

    /// Update the `transcode` opt-in on an existing upload row. Called when
    /// `confirm_staged` promotes a STAGED task to PENDING with its transcode
    /// choice captured.
    pub async fn update_upload_transcode(&self, id: &str, transcode: bool) -> Result<(), DbError> {
        let transcode = i64::from(transcode);
        sqlx::query!(
            "UPDATE upload_queue SET transcode = ?, updated_at = datetime('now') WHERE id = ?",
            transcode,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(task_id = %id, state = %state.as_str()))]
    pub async fn update_upload_state(
        &self,
        id: &str,
        state: UploadState,
        error_message: Option<&str>,
    ) -> Result<(), DbError> {
        let state_str = state.as_str();
        sqlx::query!(
            "UPDATE upload_queue SET state = ?, error_message = ?, updated_at = datetime('now') WHERE id = ?",
            state_str,
            error_message,
            id,
        )
        .execute(&self.pool)
        .await?;
        tracing::debug!("state transition");
        Ok(())
    }

    pub async fn update_upload_progress(
        &self,
        id: &str,
        bytes_uploaded: u64,
    ) -> Result<(), DbError> {
        let bytes = bytes_uploaded as i64;
        sqlx::query!(
            "UPDATE upload_queue SET bytes_uploaded = ?, updated_at = datetime('now') WHERE id = ?",
            bytes,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_upload_document_id(
        &self,
        id: &str,
        document_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue SET document_id = ?, updated_at = datetime('now') WHERE id = ?",
            document_id,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_upload_session_id(
        &self,
        id: &str,
        session_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue SET session_id = ?, updated_at = datetime('now') WHERE id = ?",
            session_id,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record the transcoded artifact size. Called once transcoding completes
    /// so the UI can render "original → transcoded" bytes even across restarts.
    pub async fn update_upload_transcoded_size(
        &self,
        id: &str,
        transcoded_size: u64,
    ) -> Result<(), DbError> {
        let size = transcoded_size as i64;
        sqlx::query!(
            "UPDATE upload_queue SET transcoded_size = ?, updated_at = datetime('now') WHERE id = ?",
            size,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persist all four legs of the staging-time hash pass in one write
    /// (BLAKE3 + MD5 + CRC32C + SHA-256-head). Used by the staging-time
    /// hash worker — separate writes would race with
    /// `update_upload_state` flipping the row to `Staged` / `Rejected`,
    /// and could leave a row half-written if the app crashes mid-update.
    /// Takes the full [`FileHashes`] by reference so the four hex
    /// strings are guaranteed to come from the same I/O pass.
    pub async fn update_upload_hashes(&self, id: &str, h: &FileHashes) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue
             SET hash = ?, source_md5 = ?, source_crc32c = ?, source_sha256_head_256kib = ?,
                 updated_at = datetime('now')
             WHERE id = ?",
            h.blake3_hex,
            h.md5_hex,
            h.crc32c_b64,
            h.sha256_head_256kib_hex,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Settle the staging-time quality-check worker in one UPDATE:
    /// state + video_info + warnings + cleared error message. Used at
    /// the `QualityChecking → Hashing` transition so the popover-data
    /// fields the response carries land atomically with the state
    /// change. `video_info` is serialised to JSON; `None` clears the
    /// column.
    pub async fn update_upload_quality_check_settled(
        &self,
        id: &str,
        state: UploadState,
        video_info: Option<&crate::models::VideoInfo>,
        warnings: &[String],
    ) -> Result<(), DbError> {
        let state_str = state.as_str();
        let video_info_json = video_info.map(serde_json::to_string).transpose()?;
        let warnings_json = serde_json::to_string(warnings)?;
        sqlx::query!(
            "UPDATE upload_queue SET state = ?, video_info = ?, validation_warnings = ?, error_message = NULL, updated_at = datetime('now') WHERE id = ?",
            state_str,
            video_info_json,
            warnings_json,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Settle the staging-time hash worker in one UPDATE: state +
    /// warnings + reasons + cleared error message. Used at the
    /// terminal `Staged` / `Rejected` transition so the row never
    /// sits in an intermediate window that the UI could observe
    /// between two separate writes.
    pub async fn update_upload_state_warnings_and_reasons(
        &self,
        id: &str,
        state: UploadState,
        warnings: &[String],
        rejection_reasons: &[String],
    ) -> Result<(), DbError> {
        let state_str = state.as_str();
        let warnings_json = serde_json::to_string(warnings)?;
        let reasons_json = serde_json::to_string(rejection_reasons)?;
        sqlx::query!(
            "UPDATE upload_queue SET state = ?, validation_warnings = ?, rejection_reasons = ?, error_message = NULL, updated_at = datetime('now') WHERE id = ?",
            state_str,
            warnings_json,
            reasons_json,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Flip the super-admin override on a task and reset it to PENDING
    /// (clearing any prior error message). Used by the "Force upload"
    /// button on rejected rows. The state move to PENDING is what
    /// re-enters the row into the upload pipeline; the flag is what
    /// makes `process_task` skip the dedup short-circuit.
    pub async fn force_upload_task(&self, id: &str) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue SET force_upload = 1, state = 'PENDING', error_message = NULL, created_at = datetime('now'), updated_at = datetime('now') WHERE id = ?",
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    pub async fn reset_stale_uploads(&self) -> Result<u64, DbError> {
        // Only the in-process staging states (HASHING, QUALITY_CHECKING) are
        // failed here: their workers run in-process and die with the app, and
        // there is no persisted mid-hash / mid-check progress to resume, so the
        // row would otherwise sit forever waiting for a verdict no living task
        // will emit (the user can re-add the file for a fresh run).
        //
        // The upload-pipeline states (PENDING / UPLOADING / CREATING / VERIFYING
        // / VALIDATING / DESENSITIZING / TRANSCODING) are deliberately NOT failed.
        // `resume_pending`, which runs right after this on startup, re-drives
        // them cleanly: PENDING just re-dispatches, UPLOADING resumes from the
        // confirmed GCS byte offset (`query_progress`), and the rest re-run their
        // stage via `process_task`. Failing them here used to defeat that resume
        // and dump a wall of false "Interrupted by app restart" rows that only
        // trickled back via the 30s network-probe auto-retry.
        let result = sqlx::query!(
            "UPDATE upload_queue SET state = 'FAILED', error_message = 'Interrupted by app restart', updated_at = datetime('now')
             WHERE state IN ('HASHING', 'QUALITY_CHECKING')",
        )
        .execute(&self.pool)
        .await?;
        let rows = result.rows_affected();
        if rows > 0 {
            tracing::warn!(rows, "reset stale in-flight uploads after restart");
        }
        Ok(rows)
    }

    /// Get failed uploads that are retryable (network errors, server errors, interrupted).
    pub async fn get_failed_retryable(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query_as!(
            UploadRow,
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload
             FROM upload_queue
             WHERE state = 'FAILED'
               AND retry_count < 10
               AND (error_message LIKE '%Network%'
                    OR error_message LIKE '%timeout%'
                    OR error_message LIKE '%no healthy upstream%'
                    OR error_message LIKE '%Interrupted%'
                    OR error_message LIKE '%error sending request%')
             ORDER BY created_at ASC
             LIMIT 20",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(UploadTask::from).collect())
    }

    pub async fn get_staged_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query_as!(
            UploadRow,
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload
             FROM upload_queue WHERE state = 'STAGED' ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(UploadTask::from).collect())
    }

    #[tracing::instrument(skip_all)]
    pub async fn get_pending_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query_as!(
            UploadRow,
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload
             FROM upload_queue
             WHERE state IN ('PENDING', 'UPLOADING', 'CREATING', 'VERIFYING', 'VALIDATING', 'DESENSITIZING', 'TRANSCODING')
             ORDER BY created_at ASC, rowid ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        if !rows.is_empty() {
            tracing::debug!(count = rows.len(), "polled pending uploads");
        }
        Ok(rows.into_iter().map(UploadTask::from).collect())
    }

    /// Load a single upload row by id. Used by the auto-advance path so a
    /// freshly-`Pending` row can be fetched in O(1) instead of loading the
    /// whole pending set and scanning it — staging 100 files would otherwise
    /// run ~100 full-table loads (one per auto-advancing task).
    pub async fn get_upload_by_id(&self, id: &str) -> Result<Option<UploadTask>, DbError> {
        let row = sqlx::query_as!(
            UploadRow,
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload
             FROM upload_queue WHERE id = ?",
            id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(UploadTask::from))
    }

    pub async fn get_all_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query_as!(
            UploadRow,
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload
             FROM upload_queue ORDER BY created_at DESC LIMIT 100",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(UploadTask::from).collect())
    }

    pub async fn delete_upload_task(&self, id: &str) -> Result<(), DbError> {
        sqlx::query!("DELETE FROM upload_queue WHERE id = ?", id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -- File Hashes (dedup) --

    pub async fn insert_file_hash(
        &self,
        hash: &str,
        filename: &str,
        size: u64,
        tenant_id: &str,
        project_id: &str,
        document_id: &str,
    ) -> Result<(), DbError> {
        let size = size as i64;
        sqlx::query!(
            "INSERT OR REPLACE INTO file_hashes (hash, filename, size, tenant_id, project_id, document_id)
             VALUES (?, ?, ?, ?, ?, ?)",
            hash, filename, size, tenant_id, project_id, document_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_by_hash(&self, hash: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query!("SELECT document_id FROM file_hashes WHERE hash = ?", hash,)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.document_id))
    }
}
