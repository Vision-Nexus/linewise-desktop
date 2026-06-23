-- The desensitize (metadata-strip) stage was removed from the upload pipeline.
-- Re-drive any row interrupted mid-desensitize back into the upload pipeline so
-- it recovers instead of sticking in a state the engine no longer produces.
-- (UploadState::parse() maps any unknown string to Pending as a backstop, but
-- converting here keeps the persisted data consistent with the new state set.)
--
-- We intentionally KEEP the now-dead `desensitized_path` column: dropping it
-- would require ALTER TABLE ... DROP COLUMN, which fails on system SQLite
-- < 3.35 (desktop links the system library, version unknown per machine) and
-- would brick startup. The column is never read or written by any query.
UPDATE upload_queue
SET state = 'PENDING', error_message = NULL, updated_at = datetime('now')
WHERE state = 'DESENSITIZING';
