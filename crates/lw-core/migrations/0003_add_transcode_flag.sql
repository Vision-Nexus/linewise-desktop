-- Persist the per-task transcode opt-in so resume-after-crash picks it up.
-- Without this, a partially-transcoded upload loses its transcode flag on
-- relaunch and silently falls through to uploading the original file.
ALTER TABLE upload_queue ADD COLUMN transcode INTEGER NOT NULL DEFAULT 0;
