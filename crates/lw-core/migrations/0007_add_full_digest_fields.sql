-- Source-file CRC32C (base64 of 4 BE bytes) and SHA-256 over the first
-- 262144 bytes. Computed alongside source_md5 + BLAKE3 in a single
-- I/O pass at staging time. Sent to linewise-api as
-- `digest.{crc32c, sha256_head_256kib}` on document creation. NULL on
-- rows created before this migration — the upload pipeline rehashes
-- legacy rows on confirm-staged so values backfill on first use.
ALTER TABLE upload_queue ADD COLUMN source_crc32c TEXT;
ALTER TABLE upload_queue ADD COLUMN source_sha256_head_256kib TEXT;
