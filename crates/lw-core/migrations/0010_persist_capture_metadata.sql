-- V11: persist capture-metadata resolution on the upload row.
--
-- The engine keeps capture state in three in-memory maps (metadata / embedded /
-- skipped), lost on restart. That is fine for a clip whose Save-time in-place
-- embed SUCCEEDED (the tags live in the file and are read back on restart), but a
-- clip whose in-place embed FAILED (read-only / full removable media) has its
-- metadata ONLY in memory — after a restart it would upload silently untagged or
-- lose the values the user entered. Persisting the resolution here lets
-- `recover_capture_for_staged` rehydrate the maps from the row.
--
-- capture_status: 'none' (default) | 'filled' | 'embedded' | 'skipped'.
--   filled   = metadata recorded, tags NOT yet in the file (a copy is tagged at upload)
--   embedded = tags are in the source file
--   skipped  = user chose to upload without capture metadata
-- capture_json: serialized CaptureMetadata for 'filled' / 'embedded', else NULL.
ALTER TABLE upload_queue ADD COLUMN capture_status TEXT NOT NULL DEFAULT 'none';
ALTER TABLE upload_queue ADD COLUMN capture_json   TEXT;
