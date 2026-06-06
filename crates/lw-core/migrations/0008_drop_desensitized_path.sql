-- Desensitization was removed from the upload pipeline. The
-- `desensitized_path` column (added in 0001) was already unmapped from the
-- code and is now dropped for a clean schema. No query references it, so the
-- sqlx offline cache in .sqlx is unaffected. Requires SQLite >= 3.35 for
-- DROP COLUMN, which the bundled libsqlite3-sys provides.

-- Normalize any row left mid-desensitize by an older build. The state no
-- longer exists in code (UploadState::parse maps unknown strings to PENDING),
-- and the restart-cleanup and resume queries no longer list 'DESENSITIZING',
-- so reset such rows to PENDING here for a clean re-run.
UPDATE upload_queue SET state = 'PENDING' WHERE state = 'DESENSITIZING';

ALTER TABLE upload_queue DROP COLUMN desensitized_path;
