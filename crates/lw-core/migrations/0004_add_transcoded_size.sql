-- Persist the size of the transcoded artifact so the UI can show
-- "original → transcoded" bytes on resumed tasks. NULL until transcode
-- completes or when transcode is disabled for the task.
ALTER TABLE upload_queue ADD COLUMN transcoded_size INTEGER;
