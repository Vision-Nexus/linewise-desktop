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
    mpu_upload_id: Option<String>,
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
    created_at: Option<String>,
    updated_at: Option<String>,
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
            mpu_upload_id: r.mpu_upload_id,
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
                .and_then(|s| serde_json::from_str(s).ok())
                .map(std::sync::Arc::new),
            force_upload: r.force_upload != 0,
            created_at: r.created_at.unwrap_or_default(),
            updated_at: r.updated_at.unwrap_or_default(),
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
        // Serialize the dereferenced `&VideoInfo`, not the `Arc` itself, so we
        // never depend on serde's `rc` feature (the field is `Arc`-wrapped only
        // to make render-time `UploadTask` clones cheap).
        let video_info_json = task
            .video_info
            .as_deref()
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

    /// Bump the durable auto-retry count for a task by one. Called by
    /// `spawn_auto_retry` immediately BEFORE launching a retry so the count
    /// survives an app restart and can't be defeated by in-process bookkeeping
    /// loss — it is the give-up axis (see `AUTO_RETRY_MAX_ATTEMPTS`).
    pub async fn increment_retry_count(&self, id: &str) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue SET retry_count = retry_count + 1, updated_at = datetime('now') WHERE id = ?",
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Zero the durable auto-retry count. Called on a successful upload and on a
    /// user-triggered manual retry, so a row starts its give-up budget fresh
    /// rather than inheriting a count that already reached the cap.
    pub async fn reset_retry_count(&self, id: &str) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue SET retry_count = 0, updated_at = datetime('now') WHERE id = ?",
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Guarded terminal COMPLETE — the fix for the terminal-revert and
    /// give-up-reopen races. Marks a row `COMPLETED` and zeroes its durable
    /// `retry_count` in ONE write, but only if the row is not ALREADY terminal
    /// (`COMPLETED` / `GAVE_UP` / `REJECTED`). Returns `true` iff this call
    /// applied it (`rows_affected == 1`).
    ///
    /// Completion is a fact about the server (the bytes are durable and
    /// verified), so this write is intentionally NOT keyed on any per-worker
    /// owner: whichever worker finished may record it. Folding the reset into the
    /// same guarded UPDATE means a lagging duplicate worker can neither revert the
    /// terminal state nor re-arm the 10-attempt give-up budget via a separate
    /// `reset_retry_count`.
    pub async fn settle_completed(&self, id: &str) -> Result<bool, DbError> {
        let result = sqlx::query!(
            "UPDATE upload_queue
                SET state = 'COMPLETED', error_message = NULL, retry_count = 0,
                    updated_at = datetime('now')
             WHERE id = ? AND state NOT IN ('COMPLETED', 'GAVE_UP', 'REJECTED', 'PAUSED')",
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Guarded terminal FAILURE — moves a row to `Failed` or `GaveUp` with a
    /// message, but refuses to overwrite a row that is ALREADY terminal, so a
    /// lagging failure write can never clobber a `COMPLETED` a sibling just
    /// recorded (a finished upload stays finished). `to` is bound, so the SQL
    /// text is static and the `query!` macro stays compile-checked. Returns
    /// `true` iff it applied.
    pub async fn settle_failure(
        &self,
        id: &str,
        to: UploadState,
        error_message: &str,
    ) -> Result<bool, DbError> {
        let state_str = to.as_str();
        let result = sqlx::query!(
            "UPDATE upload_queue
                SET state = ?, error_message = ?, updated_at = datetime('now')
             WHERE id = ? AND state NOT IN ('COMPLETED', 'GAVE_UP', 'REJECTED', 'PAUSED')",
            state_str,
            error_message,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Guarded PAUSE — moves an in-flight upload to `Paused`, but ONLY from
    /// `Uploading` (the sole legal predecessor; see `state_machine::allowed`).
    /// A user pause that races a worker finishing is a clean no-op: once the row
    /// has left `Uploading` (Verifying / Completed / Failed) this applies nothing,
    /// so the terminal outcome wins. Returns `true` iff it applied.
    pub async fn settle_paused(&self, id: &str) -> Result<bool, DbError> {
        let result = sqlx::query!(
            "UPDATE upload_queue
                SET state = 'PAUSED', updated_at = datetime('now')
             WHERE id = ? AND state = 'UPLOADING'",
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Persist a task's capture-metadata resolution so it survives a restart even
    /// when the tags never made it into the file (a failed in-place embed). The
    /// in-memory maps in `UploadEngine` remain the hot path; this is the durable
    /// backing that `recover_capture_for_staged` hydrates from. `status` is
    /// `none` / `filled` / `embedded` / `skipped`; `json` is the serialized
    /// `CaptureMetadata` for `filled` / `embedded`, `None` otherwise.
    pub async fn set_capture_row(
        &self,
        id: &str,
        status: &str,
        json: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue SET capture_status = ?, capture_json = ?, updated_at = datetime('now') WHERE id = ?",
            status,
            json,
            id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Read a task's persisted capture resolution as `(status, json)`. Used at
    /// startup by `recover_capture_for_staged` to rehydrate the in-memory maps.
    pub async fn get_capture_row(
        &self,
        id: &str,
    ) -> Result<Option<(String, Option<String>)>, DbError> {
        let row = sqlx::query!(
            "SELECT capture_status, capture_json FROM upload_queue WHERE id = ?",
            id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.capture_status, r.capture_json)))
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

    /// Persist the GCS XML Multipart Upload (MPU) `uploadId` for a task so a
    /// parallel-chunk upload can RESUME across an app restart instead of
    /// restarting from zero. Called once, right after the backend initiates
    /// the MPU and before any part PUT. Mirrors [`update_upload_session_id`].
    /// Pass `None` to clear it (e.g. when a stale/expired upload is abandoned
    /// and we fall back to a fresh MPU).
    pub async fn update_upload_mpu_upload_id(
        &self,
        id: &str,
        mpu_upload_id: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE upload_queue SET mpu_upload_id = ?, updated_at = datetime('now') WHERE id = ?",
            mpu_upload_id,
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

    /// Null the four staging-time hash columns so the upload worker re-derives
    /// the digest. Called after a Save-time capture embed rewrites the source
    /// file in place: the staging hash was taken on the untagged bytes and no
    /// longer matches what will be uploaded.
    pub async fn clear_upload_hashes(&self, id: &str) -> Result<(), DbError> {
        // Non-macro query: avoids adding a new entry to the offline `.sqlx` cache
        // (no sqlx-cli in this environment). Static SQL + one bind, so the lost
        // compile-time check is immaterial.
        sqlx::query(
            "UPDATE upload_queue
             SET hash = NULL, source_md5 = NULL, source_crc32c = NULL,
                 source_sha256_head_256kib = NULL, updated_at = datetime('now')
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Settle the staging-time quality-check worker in one UPDATE:
    /// state + video_info + warnings + cleared error message. Used at the
    /// `QualityChecking → Staged` (accept) and `QualityChecking → Rejected`
    /// (QC failure) transitions so the popover-data fields the response
    /// carries land atomically with the state change. Staging no longer
    /// passes through `Hashing` — dedup happens once, post-embed, at
    /// Stage 4. `video_info` is serialised to JSON; `None` clears the column.
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
    ///
    /// The `error_message LIKE …` allow-list below is the auto-retry gate: a
    /// `Failed` row is re-queued only when its message matches a transient
    /// transport marker. Permanent failures — a missing source file
    /// ([`crate::error::UploadError::SourceFileMissing`]), a full disk, a 4xx,
    /// a file that changed on disk — match none of these and are never
    /// auto-retried; the user resumes them manually (per-row Retry) after
    /// fixing the cause.
    pub async fn get_failed_retryable(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query_as!(
            UploadRow,
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, mpu_upload_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload, created_at, updated_at
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
                    document_id, session_id, mpu_upload_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload, created_at, updated_at
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
                    document_id, session_id, mpu_upload_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload, created_at, updated_at
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
                    document_id, session_id, mpu_upload_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload, created_at, updated_at
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
                    document_id, session_id, mpu_upload_id, bytes_uploaded, state, error_message,
                    hash, source_md5, source_crc32c, source_sha256_head_256kib, validation_warnings, rejection_reasons, retry_count, video_info, transcode, transcoded_size, force_upload, created_at, updated_at
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

    /// The earliest-created NON-terminal upload-queue row carrying this content
    /// hash — the deterministic "winner" of an in-flight de-dup. Returns its
    /// `(id, document_id)`. Used so that when the same file is staged more than
    /// once (a re-add, or a second app instance — the SQLite DB is shared, so a
    /// sibling owned by another process is visible here too), only the earliest
    /// row uploads and the rest defer as duplicates instead of each creating a
    /// new document. Terminal rows (Completed/Failed/Rejected/Paused) are
    /// excluded so a finished or abandoned attempt never blocks a fresh one.
    pub async fn find_inflight_sibling_winner(
        &self,
        hash: &str,
    ) -> Result<Option<(String, Option<String>)>, DbError> {
        let row = sqlx::query!(
            "SELECT id AS \"id!\", document_id FROM upload_queue
             WHERE hash = ?
               AND state IN ('HASHING','QUALITY_CHECKING','STAGED','PENDING','VALIDATING','TRANSCODING','DESENSITIZING','CREATING','UPLOADING','VERIFYING')
             ORDER BY created_at ASC, id ASC
             LIMIT 1",
            hash,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.id, r.document_id)))
    }
}

#[cfg(test)]
mod cas_tests {
    //! Integration tests for the guarded terminal-settle primitives, run against
    //! a real in-memory SQLite so they exercise the exact `WHERE` guards the
    //! production code relies on (more faithful than a hand-mirrored fake).
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> Database {
        // Single connection so the whole test shares one in-memory database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("run migrations");
        Database { pool }
    }

    async fn seed(db: &Database, id: &str, state: UploadState, retry_count: i64) {
        let s = state.as_str();
        sqlx::query!(
            "INSERT INTO upload_queue
                (id, local_path, filename, size, mime_type, tenant_id, project_id, state, retry_count)
             VALUES (?, '/tmp/x.mp4', 'x.mp4', 1, 'video/mp4', 't', 'p', ?, ?)",
            id,
            s,
            retry_count,
        )
        .execute(&db.pool)
        .await
        .expect("seed row");
    }

    async fn state_of(db: &Database, id: &str) -> UploadState {
        db.get_upload_by_id(id)
            .await
            .expect("get")
            .expect("row")
            .state
    }
    async fn retry_of(db: &Database, id: &str) -> u32 {
        db.get_upload_by_id(id)
            .await
            .expect("get")
            .expect("row")
            .retry_count
    }

    #[tokio::test]
    async fn settle_completed_marks_and_folds_reset() {
        // V4: the retry_count reset is folded into the guarded COMPLETED write.
        let db = test_db().await;
        seed(&db, "a", UploadState::Uploading, 7).await;
        assert!(db.settle_completed("a").await.expect("settle"));
        assert_eq!(state_of(&db, "a").await, UploadState::Completed);
        assert_eq!(retry_of(&db, "a").await, 0);
    }

    #[tokio::test]
    async fn settle_completed_is_idempotent_and_never_reverts() {
        let db = test_db().await;
        seed(&db, "a", UploadState::Verifying, 0).await;
        assert!(db.settle_completed("a").await.expect("first"));
        // Already terminal → the guard refuses the second write.
        assert!(!db.settle_completed("a").await.expect("second"));
        assert_eq!(state_of(&db, "a").await, UploadState::Completed);
    }

    #[tokio::test]
    async fn settle_paused_only_from_uploading() {
        let db = test_db().await;
        // Uploading -> Paused applies (the sole legal predecessor).
        seed(&db, "up", UploadState::Uploading, 0).await;
        assert!(db.settle_paused("up").await.expect("pause uploading"));
        assert_eq!(state_of(&db, "up").await, UploadState::Paused);
        // Any non-Uploading state is a no-op — pause is meaningful only in flight.
        seed(&db, "vf", UploadState::Verifying, 0).await;
        assert!(!db.settle_paused("vf").await.expect("pause verifying"));
        assert_eq!(state_of(&db, "vf").await, UploadState::Verifying);
    }

    #[tokio::test]
    async fn paused_row_is_not_clobbered_by_late_settle() {
        // A lagging worker that finishes/fails just after the user paused must not
        // overwrite the Paused row — the settle guards exclude PAUSED.
        let db = test_db().await;
        seed(&db, "a", UploadState::Uploading, 0).await;
        assert!(db.settle_paused("a").await.expect("pause"));
        assert!(!db.settle_completed("a").await.expect("late complete"));
        assert!(
            !db.settle_failure("a", UploadState::Failed, "net")
                .await
                .expect("late fail")
        );
        assert_eq!(state_of(&db, "a").await, UploadState::Paused);
    }

    #[tokio::test]
    async fn settle_failure_cannot_clobber_a_completed_row() {
        // V3: a lagging FAILED write must not revert a COMPLETED row.
        let db = test_db().await;
        seed(&db, "a", UploadState::Uploading, 0).await;
        assert!(db.settle_completed("a").await.expect("complete"));
        assert!(
            !db.settle_failure("a", UploadState::Failed, "net")
                .await
                .expect("late fail")
        );
        assert_eq!(state_of(&db, "a").await, UploadState::Completed);
    }

    #[tokio::test]
    async fn settle_failure_moves_active_then_locks_at_terminal() {
        let db = test_db().await;
        seed(&db, "a", UploadState::Uploading, 0).await;
        assert!(
            db.settle_failure("a", UploadState::Failed, "net")
                .await
                .expect("fail")
        );
        assert_eq!(state_of(&db, "a").await, UploadState::Failed);
        // Failed is not terminal, so give-up is allowed.
        assert!(
            db.settle_failure("a", UploadState::GaveUp, "gave up")
                .await
                .expect("giveup")
        );
        assert_eq!(state_of(&db, "a").await, UploadState::GaveUp);
        // GaveUp is terminal → further failure writes are refused.
        assert!(
            !db.settle_failure("a", UploadState::Failed, "x")
                .await
                .expect("after terminal")
        );
    }

    #[tokio::test]
    async fn capture_row_persists_and_reads_back() {
        // V11: a clip's capture resolution survives on the row (default 'none',
        // then a 'filled' write with JSON reads back intact).
        let db = test_db().await;
        seed(&db, "a", UploadState::Staged, 0).await;
        assert_eq!(
            db.get_capture_row("a").await.expect("get default"),
            Some(("none".to_string(), None))
        );
        db.set_capture_row("a", "filled", Some(r#"{"lens":"x"}"#))
            .await
            .expect("set");
        assert_eq!(
            db.get_capture_row("a").await.expect("get filled"),
            Some(("filled".to_string(), Some(r#"{"lens":"x"}"#.to_string())))
        );
    }
}
