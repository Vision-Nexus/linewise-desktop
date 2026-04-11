-- Initial schema: file hashes (dedup) and upload queue

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
