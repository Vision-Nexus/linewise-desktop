use crate::db::Database;
use crate::error::AppError;
use futures_core::Stream;
use md5::{Digest, Md5};
use std::path::Path;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

/// Compute BLAKE3 hash of a file
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

/// Final hashes for a source file: BLAKE3 powers the local-dedup
/// short-circuit, MD5 feeds the cross-tenant dedup registry in
/// linewise-api. Returned as the last `Done` item of a [`HashEvent`]
/// stream so callers can render progress without re-reading the file.
#[derive(Debug, Clone)]
pub struct FileHashes {
    pub blake3_hex: String,
    pub md5_hex: String,
}

/// Item emitted by [`hash_file_blake3_and_md5_stream`]. `Progress`
/// arrives after every 1 MiB chunk; the stream ends with exactly one
/// `Done` (success) or `Error` (I/O failure on read). Callers must
/// treat `Done` / `Error` as terminal and stop polling.
#[derive(Debug, Clone)]
pub enum HashEvent {
    Progress { bytes_so_far: u64, total_bytes: u64 },
    Done(FileHashes),
    Error(String),
}

/// Single-pass `(BLAKE3, MD5)` over the file as an event stream. The
/// hasher runs on a blocking thread and pipes events through an
/// unbounded mpsc — unbounded because the consumer is the UI / DB
/// pump, which is much faster than disk I/O in practice; bounding
/// would only matter if hashing somehow outran the consumer, and at
/// disk speeds it doesn't.
///
/// One I/O pass over the file. Streaming events is the cheap shape:
/// the caller drives the loop, progress reaches the UI in real time,
/// and there is no callback boxed across the spawn boundary.
pub fn hash_file_blake3_and_md5_stream(
    path: &Path,
) -> impl Stream<Item = HashEvent> + Send + 'static {
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
            bytes_so_far += n as u64;
            if bytes_so_far - last_emit >= 1024 * 1024 || bytes_so_far == total_bytes {
                last_emit = bytes_so_far;
                let _ = tx.send(HashEvent::Progress {
                    bytes_so_far,
                    total_bytes,
                });
            }
        }
        let _ = tx.send(HashEvent::Done(FileHashes {
            blake3_hex: blake3_hasher.finalize().to_hex().to_string(),
            md5_hex: format!("{:x}", md5_hasher.finalize()),
        }));
    });

    UnboundedReceiverStream::new(rx)
}

/// Check if a file is a duplicate based on its hash
pub async fn check_duplicate(db: &Database, path: &Path) -> Result<Option<String>, AppError> {
    let hash = hash_file(path)
        .await
        .map_err(|e| AppError::Upload(crate::error::UploadError::Io(e)))?;
    db.find_by_hash(&hash).await.map_err(AppError::Database)
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
        let mut stream = Box::pin(hash_file_blake3_and_md5_stream(path));
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
    async fn dual_hash_matches_known_vectors() {
        // RFC 1321 / blake3 reference vectors for an empty input.
        let path = write_tmp(b"", "empty");
        let h = drain_hashes(&path).await;
        let _ = std::fs::remove_file(&path);
        assert_eq!(h.md5_hex, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(
            h.blake3_hex,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[tokio::test]
    async fn dual_hash_matches_legacy_blake3() {
        let path = write_tmp(b"hello world", "hw");
        let h = drain_hashes(&path).await;
        let b3_legacy = hash_file(&path).await.expect("legacy");
        let _ = std::fs::remove_file(&path);
        assert_eq!(h.blake3_hex, b3_legacy);
    }

    #[tokio::test]
    async fn stream_emits_progress_then_done() {
        // 1.5 MiB to force at least one Progress event before Done.
        let bytes = vec![7u8; 1_572_864];
        let path = write_tmp(&bytes, "progress");
        let mut stream = Box::pin(hash_file_blake3_and_md5_stream(&path));
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
