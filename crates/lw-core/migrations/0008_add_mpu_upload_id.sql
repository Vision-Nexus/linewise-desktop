-- Persist the GCS XML Multipart Upload (MPU) uploadId so a parallel-chunk
-- upload can RESUME across an app restart instead of restarting from zero.
--
-- The uploadId is the one piece of MPU state that cannot be recovered from
-- the server: the already-uploaded parts and their ETags are re-derivable via
-- the GCS ListParts API (GET /OBJECT?uploadId=...), but ListParts needs the
-- uploadId, and GCS cannot reverse-look-it-up from the object name. Mirrors
-- the existing nullable `session_id` column used by the resumable-upload path.
-- NULL for resumable / non-GCS / pre-migration rows.
ALTER TABLE upload_queue ADD COLUMN mpu_upload_id TEXT;
