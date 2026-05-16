-- Split severity for the per-row hint list. Until now `validation_warnings`
-- carried both advisory recommendations and hard reject reasons, and the UI
-- coloured them all as warnings — which read as "you might want to check
-- this" even when the row was actually refused. From this migration on,
-- `validation_warnings` holds only advisory (warn-coloured) lines, and
-- `rejection_reasons` holds the acceptance-band reject lines that the UI
-- renders in the error palette.
ALTER TABLE upload_queue ADD COLUMN rejection_reasons TEXT;
