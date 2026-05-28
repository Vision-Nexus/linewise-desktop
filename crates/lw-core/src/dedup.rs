use crate::db::Database;
use crate::error::AppError;
use base64::Engine as _;
use futures_core::Stream;
use md5::{Digest as _, Md5};
use sha2::Sha256;
use std::path::Path;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Number of bytes hashed by the `sha256_head_256kib` leg of the digest.
/// Mirrors `linewise-api`'s `Sha256Hex` semantics — the API also computes
/// SHA-256 over a 256 KiB prefix in the GCS-finalize callback to populate
/// `verified_digest.sha256_head_256kib`. Must stay literally 262144 so a
/// desktop-supplied head and a server-supplied head over the same bytes
/// match.
pub const SHA256_HEAD_LIMIT: u64 = 256 * 1024;

/// Compute BLAKE3 hash of a file
#[tracing::instrument(skip_all, fields(filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("?")))]
pub async fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut hasher = blake3::Hasher::new();
        let file = std::fs::File::open(&path)?;
        let mut reader = std::io::BufReader::with_capacity(1024 * 1024, file);
        std::io::copy(&mut reader, &mut hasher)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .expect("hash_file task panicked")
}

/// Final hashes for a source file. Four legs computed in one I/O pass:
///
///   - `blake3_hex` powers the local-DB dedup short-circuit (separate
///     SQLite table, BLAKE3-keyed; not part of the digest sent to the
///     server).
///   - `md5_hex` is `Digest.md5` on the wire AND the key of the
///     cross-tenant dedup registry in linewise-api (the `dedup-checks`
///     POST body still ships md5 only).
///   - `crc32c_b64` is `Digest.crc32c` — base64 of 4 big-endian bytes
///     (8 chars total: `[A-Za-z0-9+/]{6}==`). Big-endian matches GCS's
///     `x-goog-hash: crc32c=` shape so this value is directly comparable
///     to the post-upload `verified_digest.crc32c`.
///   - `sha256_head_256kib_hex` is `Digest.sha256_head_256kib` — SHA-256
///     over the first 262144 bytes (or whole file if shorter), 64
///     lowercase hex characters.
///
/// Returned as the last `Done` item of a [`HashEvent`] stream so callers
/// can render progress without re-reading the file.
#[derive(Debug, Clone)]
pub struct FileHashes {
    pub blake3_hex: String,
    pub md5_hex: String,
    pub crc32c_b64: String,
    pub sha256_head_256kib_hex: String,
}

/// Item emitted by [`hash_file_full_stream`]. `Progress` arrives after
/// every 1 MiB chunk; the stream ends with exactly one `Done` (success)
/// or `Error` (I/O failure on read). Callers must treat `Done` /
/// `Error` as terminal and stop polling.
#[derive(Debug, Clone)]
pub enum HashEvent {
    Progress { bytes_so_far: u64, total_bytes: u64 },
    Done(FileHashes),
    Error(String),
}

/// Single-pass `(BLAKE3, MD5, CRC32C, SHA-256-head)` over the file as an
/// event stream. The hasher runs on a blocking thread and pipes events
/// through an unbounded mpsc — unbounded because the consumer is the UI
/// / DB pump, which is much faster than disk I/O in practice; bounding
/// would only matter if hashing somehow outran the consumer, and at
/// disk speeds it doesn't.
///
/// One I/O pass over the file. Streaming events is the cheap shape:
/// the caller drives the loop, progress reaches the UI in real time,
/// and there is no callback boxed across the spawn boundary.
///
/// Cost over the previous BLAKE3+MD5 pass: SHA-256 head is bounded at
/// 256 KiB so its contribution is constant. CRC32C streams the full
/// file; SSE4.2 / ARMv8 CRC instructions in release builds make it
/// effectively memory-bandwidth-bound — well under 1% of the BLAKE3
/// + MD5 cost per byte.
#[tracing::instrument(skip_all, fields(filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("?")))]
pub fn hash_file_full_stream(path: &Path) -> impl Stream<Item = HashEvent> + Send + 'static {
    let path = path.to_path_buf();
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::task::spawn_blocking(move || {
        let total_bytes = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) => {
                let _ = tx.send(HashEvent::Error(e.to_string()));
                return;
            }
        };
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.send(HashEvent::Error(e.to_string()));
                return;
            }
        };
        let mut blake3_hasher = blake3::Hasher::new();
        let mut md5_hasher = Md5::new();
        // CRC32C state is just a u32 fed into `crc32c_append`. Initial
        // value is 0; the crate's append API matches the GCS streaming
        // semantics directly.
        let mut crc32c_state: u32 = 0;
        // SHA-256 head is bounded — only the first SHA256_HEAD_LIMIT
        // bytes contribute. Counter is `u64` because total file size is.
        let mut sha256_head = Sha256::new();
        let mut sha256_bytes_consumed: u64 = 0;
        let mut reader = std::io::BufReader::with_capacity(1024 * 1024, file);
        let mut buf = [0u8; 64 * 1024];
        let mut bytes_so_far: u64 = 0;
        // 1 MiB throttle: sending an event per 64 KiB chunk would emit
        // ~150 events per 10 GB upload — wasteful. Coalescing to MiB
        // keeps the UI redraw rate sane while still showing real-time
        // motion on big files.
        let mut last_emit: u64 = 0;
        loop {
            let n = match std::io::Read::read(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = tx.send(HashEvent::Error(e.to_string()));
                    return;
                }
            };
            blake3_hasher.update(&buf[..n]);
            md5_hasher.update(&buf[..n]);
            crc32c_state = crc32c::crc32c_append(crc32c_state, &buf[..n]);
            // Only feed the head-prefix hasher while we're still inside
            // the first SHA256_HEAD_LIMIT bytes. The `take` math caps
            // the boundary chunk so we never overshoot by even one byte.
            if sha256_bytes_consumed < SHA256_HEAD_LIMIT {
                let remaining = SHA256_HEAD_LIMIT - sha256_bytes_consumed;
                let take = (n as u64).min(remaining) as usize;
                sha256_head.update(&buf[..take]);
                sha256_bytes_consumed += take as u64;
            }
            bytes_so_far += n as u64;
            if bytes_so_far - last_emit >= 1024 * 1024 || bytes_so_far == total_bytes {
                last_emit = bytes_so_far;
                let _ = tx.send(HashEvent::Progress {
                    bytes_so_far,
                    total_bytes,
                });
            }
        }
        // Big-endian is load-bearing: GCS's `x-goog-hash: crc32c=` is
        // base64 of 4 BE bytes. `to_le_bytes()` here would silently
        // never match the GCS-callback `verified_digest.crc32c`.
        let crc32c_b64 =
            base64::engine::general_purpose::STANDARD.encode(crc32c_state.to_be_bytes());
        let _ = tx.send(HashEvent::Done(FileHashes {
            blake3_hex: blake3_hasher.finalize().to_hex().to_string(),
            md5_hex: format!("{:x}", md5_hasher.finalize()),
            crc32c_b64,
            sha256_head_256kib_hex: format!("{:x}", sha256_head.finalize()),
        }));
    });

    UnboundedReceiverStream::new(rx)
}

/// Check if a file is a duplicate based on its hash
#[tracing::instrument(skip_all, fields(filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("?")))]
pub async fn check_duplicate(db: &Database, path: &Path) -> Result<Option<String>, AppError> {
    let hash = hash_file(path)
        .await
        .map_err(|e| AppError::Upload(crate::error::UploadError::Io(e)))?;
    let found = db.find_by_hash(&hash).await.map_err(AppError::Database)?;
    tracing::debug!(found = found.is_some(), "dedup local-cache check");
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tokio_stream::StreamExt;

    /// Write `bytes` to a unique path under the platform tempdir and
    /// return that path. The caller is responsible for cleanup; tests
    /// in this module are short-lived and the OS reclaims the file.
    fn write_tmp(bytes: &[u8], suffix: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "lw-core-dedup-test-{}-{suffix}",
            uuid::Uuid::new_v4()
        ));
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(bytes).expect("write");
        path
    }

    /// Drain the hash stream to its terminal event, returning the
    /// final hashes. Test-only — production code consumes Progress
    /// events as they arrive.
    async fn drain_hashes(path: &Path) -> FileHashes {
        let mut stream = Box::pin(hash_file_full_stream(path));
        let mut last_progress: Option<(u64, u64)> = None;
        while let Some(event) = stream.next().await {
            match event {
                HashEvent::Progress {
                    bytes_so_far,
                    total_bytes,
                } => last_progress = Some((bytes_so_far, total_bytes)),
                HashEvent::Done(h) => {
                    if let Some((b, t)) = last_progress {
                        assert_eq!(b, t, "final progress event must reach total");
                    }
                    return h;
                }
                HashEvent::Error(e) => panic!("hash error: {e}"),
            }
        }
        panic!("hash stream ended without Done");
    }

    #[tokio::test]
    async fn full_hash_matches_known_vectors() {
        // RFC 1321 / blake3 / NIST reference vectors for an empty input.
        // `crc32c` of zero bytes is 0 → 4 BE zero bytes → "AAAAAA==".
        let path = write_tmp(b"", "empty");
        let h = drain_hashes(&path).await;
        let _ = std::fs::remove_file(&path);
        assert_eq!(h.md5_hex, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(
            h.blake3_hex,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(h.crc32c_b64, "AAAAAA==");
        assert_eq!(
            h.sha256_head_256kib_hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn full_hash_matches_legacy_blake3() {
        let path = write_tmp(b"hello world", "hw");
        let h = drain_hashes(&path).await;
        let b3_legacy = hash_file(&path).await.expect("legacy");
        let _ = std::fs::remove_file(&path);
        assert_eq!(h.blake3_hex, b3_legacy);
    }

    #[tokio::test]
    async fn crc32c_endianness_locked() {
        // Castagnoli check value: CRC32C of "123456789" is 0xE3069283.
        // BE bytes [0xE3, 0x06, 0x92, 0x83] → base64 "4waSgw==".
        // Pins both the polynomial AND the byte order — a `to_le_bytes`
        // regression here is silent in production but caught here.
        let path = write_tmp(b"123456789", "crc-check");
        let h = drain_hashes(&path).await;
        let _ = std::fs::remove_file(&path);
        assert_eq!(h.crc32c_b64, "4waSgw==");
    }

    #[tokio::test]
    async fn sha256_head_clamps_at_256kib() {
        // 300 KiB so we cross the SHA256_HEAD_LIMIT boundary mid-file.
        // `0xAB` is arbitrary — we only care that the head hasher saw
        // exactly the first 262144 bytes, no more.
        let bytes = vec![0xABu8; 300 * 1024];
        let path = write_tmp(&bytes, "head-clamp");
        let h = drain_hashes(&path).await;
        let _ = std::fs::remove_file(&path);
        let expected = format!("{:x}", Sha256::digest(&bytes[..SHA256_HEAD_LIMIT as usize]));
        assert_eq!(h.sha256_head_256kib_hex, expected);
    }

    #[tokio::test]
    async fn sha256_head_short_file() {
        // File shorter than 256 KiB → head hash covers the whole file.
        let bytes = vec![0x42u8; 1024];
        let path = write_tmp(&bytes, "head-short");
        let h = drain_hashes(&path).await;
        let _ = std::fs::remove_file(&path);
        let expected = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(h.sha256_head_256kib_hex, expected);
    }

    #[tokio::test]
    async fn stream_emits_progress_then_done() {
        // 1.5 MiB to force at least one Progress event before Done.
        let bytes = vec![7u8; 1_572_864];
        let path = write_tmp(&bytes, "progress");
        let mut stream = Box::pin(hash_file_full_stream(&path));
        let mut progress_count = 0;
        let mut got_done = false;
        while let Some(event) = stream.next().await {
            match event {
                HashEvent::Progress { .. } => progress_count += 1,
                HashEvent::Done(_) => {
                    got_done = true;
                    break;
                }
                HashEvent::Error(e) => panic!("error: {e}"),
            }
        }
        let _ = std::fs::remove_file(&path);
        assert!(got_done, "stream did not reach Done");
        assert!(
            progress_count >= 1,
            "expected ≥1 progress event, got {progress_count}"
        );
    }
}
