-- Source-file MD5 (pre-transcode) for the cross-tenant dedup registry
-- in linewise-api. Computed alongside BLAKE3 in a single I/O pass at
-- staging time. NULL on rows created before this migration.
ALTER TABLE upload_queue ADD COLUMN source_md5 TEXT;

-- Super-admin override flag: when set, the upload engine bypasses both
-- the dedup gate and the local-DB duplicate short-circuit on this task.
-- Persisted so a re-staged row keeps its bypass across app restart.
ALTER TABLE upload_queue ADD COLUMN force_upload INTEGER NOT NULL DEFAULT 0;
