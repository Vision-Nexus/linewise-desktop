use crate::config::AppConfig;
use crate::error::DbError;
use crate::models::{UploadState, UploadTask};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

pub struct Database {
    pool: SqlitePool,
}

const INIT_SQL: &str = "
CREATE TABLE IF NOT EXISTS file_hashes (
    hash TEXT PRIMARY KEY,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    document_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS upload_queue (
    id TEXT PRIMARY KEY,
    local_path TEXT NOT NULL,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    mime_type TEXT NOT NULL,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    document_id TEXT,
    session_id TEXT,
    bytes_uploaded INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'PENDING',
    error_message TEXT,
    hash TEXT,
    validation_warnings TEXT,
    desensitized_path TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_upload_queue_state ON upload_queue(state);
";

impl Database {
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

        sqlx::raw_sql(INIT_SQL)
            .execute(&pool)
            .await
            .map_err(DbError::Sqlite)?;

        Ok(Self { pool })
    }

    // -- Upload Queue --

    pub async fn insert_upload_task(&self, task: &UploadTask) -> Result<(), DbError> {
        let warnings_json = serde_json::to_string(&task.validation_warnings)?;
        sqlx::query(
            "INSERT INTO upload_queue (id, local_path, filename, size, mime_type, tenant_id, project_id, state, hash, validation_warnings)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&task.id)
        .bind(&task.local_path)
        .bind(&task.filename)
        .bind(task.size as i64)
        .bind(&task.mime_type)
        .bind(&task.tenant_id)
        .bind(&task.project_id)
        .bind(task.state.as_str())
        .bind(&task.hash)
        .bind(&warnings_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_upload_state(
        &self,
        id: &str,
        state: UploadState,
        error_message: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE upload_queue SET state = $1, error_message = $2, updated_at = datetime('now') WHERE id = $3",
        )
        .bind(state.as_str())
        .bind(error_message)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_upload_progress(
        &self,
        id: &str,
        bytes_uploaded: u64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE upload_queue SET bytes_uploaded = $1, updated_at = datetime('now') WHERE id = $2",
        )
        .bind(bytes_uploaded as i64)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_upload_document_id(
        &self,
        id: &str,
        document_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE upload_queue SET document_id = $1, updated_at = datetime('now') WHERE id = $2",
        )
        .bind(document_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_upload_session_id(
        &self,
        id: &str,
        session_id: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE upload_queue SET session_id = $1, updated_at = datetime('now') WHERE id = $2",
        )
        .bind(session_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reset any in-progress uploads to FAILED on startup.
    /// These were interrupted by app exit and can't be resumed without re-initiating.
    pub async fn reset_stale_uploads(&self) -> Result<u64, DbError> {
        let result = sqlx::query(
            "UPDATE upload_queue SET state = 'FAILED', error_message = 'Interrupted by app restart', updated_at = datetime('now')
             WHERE state IN ('UPLOADING', 'CREATING', 'VERIFYING', 'VALIDATING', 'DESENSITIZING', 'PENDING')",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn get_staged_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query(
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, validation_warnings, retry_count
             FROM upload_queue
             WHERE state = 'STAGED'
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(row_to_task).collect())
    }

    pub async fn get_pending_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query(
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, validation_warnings, retry_count
             FROM upload_queue
             WHERE state IN ('PENDING', 'UPLOADING', 'CREATING', 'VERIFYING', 'VALIDATING', 'DESENSITIZING')
             ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(row_to_task).collect())
    }

    pub async fn get_all_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let rows = sqlx::query(
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, validation_warnings, retry_count
             FROM upload_queue
             ORDER BY created_at DESC
             LIMIT 100",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(row_to_task).collect())
    }

    pub async fn delete_upload_task(&self, id: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM upload_queue WHERE id = $1")
            .bind(id)
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
        sqlx::query(
            "INSERT OR REPLACE INTO file_hashes (hash, filename, size, tenant_id, project_id, document_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(hash)
        .bind(filename)
        .bind(size as i64)
        .bind(tenant_id)
        .bind(project_id)
        .bind(document_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn find_by_hash(&self, hash: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query("SELECT document_id FROM file_hashes WHERE hash = $1")
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| r.get("document_id")))
    }
}

fn row_to_task(row: &sqlx::sqlite::SqliteRow) -> UploadTask {
    let warnings_str: String = row
        .try_get::<Option<String>, _>("validation_warnings")
        .ok()
        .flatten()
        .unwrap_or_default();
    let warnings: Vec<String> = serde_json::from_str(&warnings_str).unwrap_or_default();
    let size: i64 = row.get("size");
    let bytes_uploaded: i64 = row.get("bytes_uploaded");
    let retry_count: i32 = row.get("retry_count");

    UploadTask {
        id: row.get("id"),
        local_path: row.get("local_path"),
        filename: row.get("filename"),
        size: size as u64,
        mime_type: row.get("mime_type"),
        tenant_id: row.get("tenant_id"),
        project_id: row.get("project_id"),
        document_id: row.get("document_id"),
        session_id: row.get("session_id"),
        bytes_uploaded: bytes_uploaded as u64,
        state: UploadState::parse(row.get::<&str, _>("state")),
        error_message: row.get("error_message"),
        hash: row.get("hash"),
        validation_warnings: warnings,
        retry_count: retry_count as u32,
    }
}
