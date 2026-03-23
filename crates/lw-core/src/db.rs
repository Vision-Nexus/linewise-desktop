use crate::config::AppConfig;
use crate::error::DbError;
use crate::models::{UploadState, UploadTask};
use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};
use std::sync::{Arc, Mutex};

pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(
            "CREATE TABLE IF NOT EXISTS file_hashes (
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

            CREATE INDEX IF NOT EXISTS idx_upload_queue_state ON upload_queue(state);",
        ),
    ])
}

impl Database {
    pub fn open() -> Result<Self, DbError> {
        let db_path = AppConfig::db_path();
        std::fs::create_dir_all(db_path.parent().expect("db path must have parent"))
            .map_err(|e| DbError::Migration(e.to_string()))?;

        let mut conn = Connection::open(&db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        migrations()
            .to_latest(&mut conn)
            .map_err(|e| DbError::Migration(e.to_string()))?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // -- Upload Queue --

    pub fn insert_upload_task(&self, task: &UploadTask) -> Result<(), DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let warnings_json = serde_json::to_string(&task.validation_warnings)?;
        conn.execute(
            "INSERT INTO upload_queue (id, local_path, filename, size, mime_type, tenant_id, project_id, state, hash, validation_warnings)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                task.id,
                task.local_path,
                task.filename,
                task.size,
                task.mime_type,
                task.tenant_id,
                task.project_id,
                task.state.as_str(),
                task.hash,
                warnings_json,
            ],
        )?;
        Ok(())
    }

    pub fn update_upload_state(
        &self,
        id: &str,
        state: UploadState,
        error_message: Option<&str>,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE upload_queue SET state = ?1, error_message = ?2, updated_at = datetime('now') WHERE id = ?3",
            rusqlite::params![state.as_str(), error_message, id],
        )?;
        Ok(())
    }

    pub fn update_upload_progress(&self, id: &str, bytes_uploaded: u64) -> Result<(), DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE upload_queue SET bytes_uploaded = ?1, updated_at = datetime('now') WHERE id = ?2",
            rusqlite::params![bytes_uploaded, id],
        )?;
        Ok(())
    }

    pub fn update_upload_document_id(
        &self,
        id: &str,
        document_id: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE upload_queue SET document_id = ?1, updated_at = datetime('now') WHERE id = ?2",
            rusqlite::params![document_id, id],
        )?;
        Ok(())
    }

    pub fn update_upload_session_id(
        &self,
        id: &str,
        session_id: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "UPDATE upload_queue SET session_id = ?1, updated_at = datetime('now') WHERE id = ?2",
            rusqlite::params![session_id, id],
        )?;
        Ok(())
    }

    pub fn get_pending_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, validation_warnings, retry_count
             FROM upload_queue
             WHERE state IN ('PENDING', 'UPLOADING', 'CREATING', 'VERIFYING', 'VALIDATING', 'DESENSITIZING')
             ORDER BY created_at ASC",
        )?;

        let tasks = stmt
            .query_map([], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn get_all_uploads(&self) -> Result<Vec<UploadTask>, DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, local_path, filename, size, mime_type, tenant_id, project_id,
                    document_id, session_id, bytes_uploaded, state, error_message,
                    hash, validation_warnings, retry_count
             FROM upload_queue
             ORDER BY created_at DESC
             LIMIT 100",
        )?;

        let tasks = stmt
            .query_map([], row_to_task)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn delete_upload_task(&self, id: &str) -> Result<(), DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute("DELETE FROM upload_queue WHERE id = ?1", [id])?;
        Ok(())
    }

    // -- File Hashes (dedup) --

    pub fn insert_file_hash(
        &self,
        hash: &str,
        filename: &str,
        size: u64,
        tenant_id: &str,
        project_id: &str,
        document_id: &str,
    ) -> Result<(), DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        conn.execute(
            "INSERT OR REPLACE INTO file_hashes (hash, filename, size, tenant_id, project_id, document_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![hash, filename, size, tenant_id, project_id, document_id],
        )?;
        Ok(())
    }

    pub fn find_by_hash(&self, hash: &str) -> Result<Option<String>, DbError> {
        let conn = self.conn.lock().expect("db mutex poisoned");
        let mut stmt =
            conn.prepare("SELECT document_id FROM file_hashes WHERE hash = ?1")?;
        let result = stmt.query_row([hash], |row| row.get(0)).ok();
        Ok(result)
    }
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<UploadTask> {
    let warnings_str: String = row.get::<_, Option<String>>(13)?.unwrap_or_default();
    let warnings: Vec<String> = serde_json::from_str(&warnings_str).unwrap_or_default();
    Ok(UploadTask {
        id: row.get(0)?,
        local_path: row.get(1)?,
        filename: row.get(2)?,
        size: row.get(3)?,
        mime_type: row.get(4)?,
        tenant_id: row.get(5)?,
        project_id: row.get(6)?,
        document_id: row.get(7)?,
        session_id: row.get(8)?,
        bytes_uploaded: row.get(9)?,
        state: UploadState::parse(&row.get::<_, String>(10)?),
        error_message: row.get(11)?,
        hash: row.get(12)?,
        validation_warnings: warnings,
        retry_count: row.get(14)?,
    })
}
