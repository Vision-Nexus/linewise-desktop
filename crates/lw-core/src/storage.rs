//! Cloud-agnostic resumable upload abstraction.
//!
//! Two backends: GCS (via signed URLs from Linewise API) and S3-compatible
//! (covers AWS S3, Alibaba OSS, Tencent COS, MinIO, etc.).

use crate::error::UploadError;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Default chunk size: 32 MiB (must be multiple of 256 KiB for GCS).
/// Larger chunks = fewer requests = faster for big video files.
const DEFAULT_CHUNK_SIZE: u64 = 32 * 1024 * 1024;

/// GCS resumable uploads require every non-final chunk to be a multiple of
/// 256 KiB. See <https://cloud.google.com/storage/docs/performing-resumable-uploads>.
const CHUNK_QUANTUM: u64 = 256 * 1024;

/// Lower bound for an auto-selected chunk. Matches the historical 8 MiB default
/// and keeps small/medium files from over-chunking.
const MIN_AUTO_CHUNK_SIZE: u64 = 8 * 1024 * 1024;

/// Upper bound for an auto-selected chunk. This caps memory: the resumable loop
/// holds one chunk in memory at a time (buffered twice — the read buffer plus
/// the per-request body copy), so peak RAM is roughly `2 * chunk * concurrent
/// files`. 64 MiB keeps a few concurrent uploads well-bounded while still
/// amortizing the per-chunk round-trip / TCP slow-start cost over a big file.
const MAX_AUTO_CHUNK_SIZE: u64 = 64 * 1024 * 1024;

/// Number of chunks to aim for, so the chunk size scales with the file instead
/// of being a fixed value: bigger files get bigger chunks (fewer round-trips),
/// always clamped to `[MIN_AUTO_CHUNK_SIZE, MAX_AUTO_CHUNK_SIZE]`.
const TARGET_CHUNK_COUNT: u64 = 32;

/// Pick a resumable chunk size for a file of `file_size` bytes.
///
/// A fixed small chunk (the old 8 MiB default) makes a multi-GB upload pay a
/// per-chunk round-trip + TCP slow-start penalty hundreds of times and never
/// saturate the link; a single whole-file chunk maximises throughput but makes a
/// late failure re-send everything. This scales the chunk with the file — aim
/// for about `TARGET_CHUNK_COUNT` chunks, clamp to `[floor, MAX_AUTO_CHUNK_SIZE]`
/// and round up to the GCS 256 KiB quantum — so throughput is recovered while a
/// failed chunk only costs one chunk to re-send.
///
/// `floor` is the configured `chunk_size_mb` in bytes: it raises the lower bound
/// (a power user can force larger chunks) and, if set above the auto cap,
/// overrides it. A zero `floor` falls back to `MIN_AUTO_CHUNK_SIZE`.
///
/// Changing this value between runs is safe: resumable uploads resume from the
/// server-confirmed byte offset (`query_progress`), not from a chunk index, so
/// the next chunk size is independent of the previous one.
pub fn pick_chunk_size(file_size: u64, floor: u64) -> u64 {
    let floor = floor.max(MIN_AUTO_CHUNK_SIZE);
    let ceil = MAX_AUTO_CHUNK_SIZE.max(floor);
    let scaled = file_size.div_ceil(TARGET_CHUNK_COUNT).clamp(floor, ceil);
    // Round up to the 256 KiB quantum GCS requires for non-final chunks.
    scaled.div_ceil(CHUNK_QUANTUM) * CHUNK_QUANTUM
}

/// Handle to an in-progress resumable upload session, persisted in SQLite
#[derive(Debug, Clone)]
pub struct UploadSession {
    pub session_id: String,
    pub total_size: u64,
    pub bytes_confirmed: u64,
}

/// One presigned part PUT for the parallel multipart (XML MPU) path. The
/// storage layer stays free of the wire DTOs in `models.rs`: `upload.rs`
/// maps each `MultipartPartUrl` from the backend plan into one of these.
#[derive(Debug, Clone)]
pub struct PartUrl {
    /// 1-based part index. Determines the file offset `(part_number - 1) * part_size`.
    pub part_number: u32,
    /// Signed PUT URL for this part's bytes.
    pub url: String,
}

/// Progress callback signature
pub type ProgressFn = Box<dyn Fn(u64, u64) + Send + Sync>;

/// Cloud storage backend — enum dispatch, no dyn trait needed
pub enum StorageBackend {
    Gcs(GcsBackend),
    S3(S3Backend),
}

impl StorageBackend {
    pub async fn initiate_upload(
        &self,
        signed_url: &str,
        content_type: &str,
        total_size: u64,
    ) -> Result<UploadSession, UploadError> {
        match self {
            Self::Gcs(b) => {
                b.initiate_upload(signed_url, content_type, total_size)
                    .await
            }
            Self::S3(b) => {
                b.initiate_upload(signed_url, content_type, total_size)
                    .await
            }
        }
    }

    pub async fn upload_chunk(
        &self,
        session: &UploadSession,
        data: &[u8],
        offset: u64,
    ) -> Result<u64, UploadError> {
        match self {
            Self::Gcs(b) => b.upload_chunk(session, data, offset).await,
            Self::S3(b) => b.upload_chunk(session, data, offset).await,
        }
    }

    pub async fn query_progress(&self, session: &UploadSession) -> Result<u64, UploadError> {
        match self {
            Self::Gcs(b) => b.query_progress(session).await,
            Self::S3(b) => b.query_progress(session).await,
        }
    }

    pub async fn abort_upload(&self, session: &UploadSession) -> Result<(), UploadError> {
        match self {
            Self::Gcs(b) => b.abort_upload(session).await,
            Self::S3(b) => b.abort_upload(session).await,
        }
    }

    /// The backend's own upload client. The connectivity probe in
    /// [`wait_for_network`] reuses it so the check follows the same
    /// (proxy-aware) path as the chunk/part PUTs.
    fn client(&self) -> &reqwest::Client {
        match self {
            Self::Gcs(b) => &b.client,
            Self::S3(b) => &b.client,
        }
    }
}

/// Upload a file with chunked resumable protocol.
#[tracing::instrument(skip_all, fields(
    filename = file_path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
    total_size = session.total_size,
    start_offset,
))]
pub async fn upload_file_chunked(
    backend: &StorageBackend,
    session: &UploadSession,
    file_path: &Path,
    start_offset: u64,
    chunk_size: u64,
    max_retries: u32,
    on_progress: &ProgressFn,
) -> Result<u64, UploadError> {
    let mut file = tokio::fs::File::open(file_path)
        .await
        .map_err(|e| UploadError::from_source_io(e, file_path))?;
    let total = session.total_size;
    let mut offset = start_offset;
    let chunk_size = if chunk_size > 0 {
        chunk_size
    } else {
        DEFAULT_CHUNK_SIZE
    };

    if start_offset > 0 {
        file.seek(std::io::SeekFrom::Start(start_offset)).await?;
    }

    // `total` (the size declared to GCS for this resumable session) is
    // immutable. If the file on disk is no longer that size, it was mutated
    // after the size snapshot — still recording, a cloud-sync placeholder, or an
    // AV scan — and uploading would short-read or finalize a truncated object.
    // Bail with an actionable, non-retryable error instead of failing mid-stream.
    let current_len = file.metadata().await?.len();
    if current_len != total {
        return Err(UploadError::FileChangedDuringUpload {
            declared: total,
            actual: current_len,
        });
    }

    while offset < total {
        let this_chunk = (total - offset).min(chunk_size) as usize;
        let mut buf = vec![0u8; this_chunk];
        // A short read means the file shrank mid-upload (the snapshot guard above
        // only catches pre-loop drift). Surface it as a file-changed outcome
        // rather than a bare "early eof" IO error that floods Sentry and auto-retries.
        if let Err(e) = file.read_exact(&mut buf).await {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                let actual = file.metadata().await.map(|m| m.len()).unwrap_or(offset);
                return Err(UploadError::FileChangedDuringUpload {
                    declared: total,
                    actual,
                });
            }
            return Err(e.into());
        }

        let confirmed =
            upload_chunk_with_retry(backend, session, &buf, offset, max_retries).await?;
        offset = confirmed;
        on_progress(offset, total);
    }

    Ok(offset)
}

/// Default per-chunk retry budget for the bounded-parallel upload path — the
/// only dispatch path. Near-infinite (100): a transient network blip should not
/// lose a chunk. The backoff plateaus at `MAX_RETRY_DELAY_MS`, so 100 attempts
/// is bounded patience (tens of minutes), not a busy-loop. Bounded concurrency
/// keeps one dead file from starving the others, so every upload uses this full
/// budget; there is no fast-fail mode.
pub const DEFAULT_MAX_RETRIES: u32 = 100;

/// First retry delay (ms) for the capped exponential backoff shared by the
/// resumable chunk path and the multipart part path.
const INITIAL_RETRY_DELAY_MS: u64 = 1000;

/// Backoff plateau (ms). Caps the exponential so a near-infinite `max_retries`
/// keeps retrying at a sane fixed interval instead of overflowing `1s << n` or
/// sleeping for days. Backoff: 1s, 2s, 4s, 8s, 16s, then a 30s plateau.
const MAX_RETRY_DELAY_MS: u64 = 30_000;

/// Compute the capped exponential backoff delay for a 1-based `attempt`
/// (attempt 1 → 1s, 2 → 2s, … 6+ → 30s). Shared by both retry loops so the
/// resumable and multipart paths back off identically.
fn retry_delay_ms(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(5);
    (INITIAL_RETRY_DELAY_MS << shift).min(MAX_RETRY_DELAY_MS)
}

/// Wall-clock ceiling for a single chunk/part's retry loop. `max_attempts`
/// alone let one chunk grind for hours: when each attempt stalled to its full
/// request timeout before failing, 100 attempts × (timeout + 30s backoff)
/// spanned most of a night while holding one of only `max_concurrent` upload
/// slots — so every other queued file sat at PENDING behind it. Capping
/// wall-clock makes a stuck chunk give up promptly and surface as a
/// retryable failure instead of stranding the whole queue.
const MAX_RETRY_WALL_CLOCK: std::time::Duration = std::time::Duration::from_secs(600);

/// Combined attempt-count + wall-clock bound for the shared upload retry loops.
/// The wall-clock axis is the real fix; `max_attempts` is kept as a secondary
/// cap so a fast-failing loop still terminates.
#[derive(Clone, Copy)]
struct RetryBudget {
    max_attempts: u32,
    max_elapsed: std::time::Duration,
}

impl RetryBudget {
    /// May we make another attempt? `attempts_made` is how many attempts have
    /// already failed; `elapsed` is wall-clock since the first attempt began.
    /// Stops as soon as either axis is exhausted.
    fn allows_retry(&self, attempts_made: u32, elapsed: std::time::Duration) -> bool {
        attempts_made < self.max_attempts && elapsed < self.max_elapsed
    }
}

/// Run `op` with the shared retry policy: retry retryable failures with capped
/// exponential backoff until `budget` is exhausted (attempt count OR wall-clock),
/// then surface the last error. A terminal (non-retryable) error returns
/// immediately. `sleep` is the inter-attempt wait — production passes
/// [`wait_for_network`]; tests pass a no-op so they neither wait nor hit the
/// network. `label` names the operation in the retry warning. Shared by the
/// resumable-chunk and multipart-part paths so both honour the same budget.
async fn retry_io<T, F, Fut, S, SFut>(
    budget: RetryBudget,
    label: &str,
    mut op: F,
    sleep: S,
) -> Result<T, UploadError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, UploadError>>,
    S: Fn(u64) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    let started = std::time::Instant::now();
    let mut attempt = 0;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(e) if is_retryable(&e) && budget.allows_retry(attempt, started.elapsed()) => {
                attempt += 1;
                let delay = retry_delay_ms(attempt);
                tracing::warn!(
                    "{label} failed (attempt {attempt}/{}), retrying in {delay}ms: {e}",
                    budget.max_attempts
                );
                sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Default bounded concurrency for the parallel multipart (XML MPU) upload
/// path: the driver uploads at most this many parts at once. Six keeps a
/// multi-GB upload saturating a fast link without fanning out to one TCP
/// connection per part (which would thrash a slow/metered link and balloon peak
/// RAM to `parts_in_flight * part_size`). This is now the *default* only —
/// `GcsBackend` carries a configured value (`UploadConfig::mpu_part_concurrency`)
/// so weak-network users can lower it to 1–2; the fallback here is used when a
/// caller constructs a backend without a config (`GcsBackend::default`) and as a
/// floor when a config value of 0 slips through.
pub(crate) const MPU_PART_CONCURRENCY: usize = 6;

/// Upload a single chunk with automatic retry on network errors.
/// Exponential backoff capped at a 30s plateau (1s, 2s, 4s, 8s, 16s, 30s, ...),
/// up to `max_retries` attempts (0 = fail fast, no retry).
/// Only retries on network/timeout errors, not on 4xx API errors.
async fn upload_chunk_with_retry(
    backend: &StorageBackend,
    session: &UploadSession,
    data: &[u8],
    offset: u64,
    max_retries: u32,
) -> Result<u64, UploadError> {
    let budget = RetryBudget {
        max_attempts: max_retries,
        max_elapsed: MAX_RETRY_WALL_CLOCK,
    };
    // `wait_for_network` polls connectivity before each backoff sleep, through
    // the backend's own (proxy-aware) client so the probe path matches the PUTs.
    let probe = backend.client();
    retry_io(
        budget,
        "Chunk upload",
        || backend.upload_chunk(session, data, offset),
        |ms| wait_for_network(probe, ms),
    )
    .await
}

/// Check if an error is retryable (network/timeout, not auth/client errors)
fn is_retryable(err: &UploadError) -> bool {
    match err {
        UploadError::Network(_) => true,
        UploadError::Api { status, .. } => {
            // Retry on 5xx server errors and 429 rate limit
            *status >= 500 || *status == 429 || *status == 408
        }
        UploadError::GcsUpload { .. } => true,
        // A file that changed on disk keeps failing until the user re-adds it
        // once stable — never auto-retry a moving target.
        UploadError::FileChangedDuringUpload { .. } => false,
        _ => false,
    }
}

/// Wait for network recovery with exponential backoff.
///
/// `client` is the backend's own proxy-aware upload client (see
/// [`GcsBackend::new`]). Probing through it — rather than a fresh
/// `reqwest::Client::new()` — means the connectivity check follows the exact
/// same path the chunk/part PUTs take: a user who set `proxy_url` has their
/// probe tunnel through v2ray's HTTP inbound too, instead of hitting
/// `storage.googleapis.com` directly (which is what wedges on CN/HK links) and
/// reporting a false "network back" the moment the direct route flickers.
async fn wait_for_network(client: &reqwest::Client, max_wait_ms: u64) {
    let check_interval = std::time::Duration::from_secs(2);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(max_wait_ms);

    // First, sleep the backoff duration
    tokio::time::sleep(std::time::Duration::from_millis(max_wait_ms)).await;

    // Then poll for connectivity if still before a reasonable deadline
    let extended_deadline = deadline + std::time::Duration::from_secs(120);
    loop {
        if tokio::time::Instant::now() > extended_deadline {
            break; // Give up waiting, let the retry logic handle it
        }
        match client
            .head("https://storage.googleapis.com")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(_) => break, // Network is back
            Err(_) => {
                tracing::debug!("Waiting for network recovery...");
                tokio::time::sleep(check_interval).await;
            }
        }
    }
}

// ── GCS Backend ────────────────────────────────────────────────────────

pub struct GcsBackend {
    client: reqwest::Client,
    /// How many parts of one MPU upload run concurrently. Configured from
    /// `UploadConfig::mpu_part_concurrency` (see [`Self::new`]); clamped to at
    /// least 1 so a misconfigured 0 can never deadlock the semaphore.
    part_concurrency: usize,
}

impl Default for GcsBackend {
    fn default() -> Self {
        Self::new(None, MPU_PART_CONCURRENCY)
    }
}

impl GcsBackend {
    /// Build the GCS upload client. `proxy` is the optional fixed proxy URL
    /// from config (`ServerConfig::proxy_url`); empty/`None` keeps the
    /// historical no-explicit-proxy behaviour. The 5-minute total timeout is
    /// deliberately generous for large resumable chunk PUTs; the 30s connect
    /// timeout fails a dead/wrong proxy fast instead of hanging the session.
    /// The 60s read timeout breaks a half-open connection (peer silently gone)
    /// in seconds and lets the retry loop recover, instead of letting one
    /// stalled PUT hold an upload slot for the full 5-minute total budget.
    ///
    /// `part_concurrency` is how many parts of one MPU upload run at once
    /// (`UploadConfig::mpu_part_concurrency`); it is clamped to at least 1 so a
    /// misconfigured 0 can never build a zero-permit semaphore that deadlocks.
    pub fn new(proxy: Option<&str>, part_concurrency: usize) -> Self {
        let client = crate::net::build_http_client(
            proxy,
            Some(std::time::Duration::from_secs(300)),
            std::time::Duration::from_secs(30),
            Some(std::time::Duration::from_secs(60)),
        )
        .expect("failed to build reqwest client");
        Self {
            client,
            part_concurrency: part_concurrency.max(1),
        }
    }

    async fn initiate_upload(
        &self,
        signed_url: &str,
        content_type: &str,
        total_size: u64,
    ) -> Result<UploadSession, UploadError> {
        let resp = self
            .client
            .post(signed_url)
            .header("x-goog-resumable", "start")
            .header(CONTENT_TYPE, content_type)
            .header(CONTENT_LENGTH, "0")
            .send()
            .await?;

        if !resp.status().is_success() && resp.status().as_u16() != 201 {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: format!(
                    "GCS initiate failed: {}",
                    resp.text().await.unwrap_or_default()
                ),
            });
        }

        let session_uri = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| UploadError::Api {
                status: 500,
                message: "GCS did not return Location header".to_string(),
            })?
            .to_string();

        Ok(UploadSession {
            session_id: session_uri,
            total_size,
            bytes_confirmed: 0,
        })
    }

    async fn upload_chunk(
        &self,
        session: &UploadSession,
        data: &[u8],
        offset: u64,
    ) -> Result<u64, UploadError> {
        let end = offset + data.len() as u64 - 1;
        let total = session.total_size;

        let resp = self
            .client
            .put(&session.session_id)
            .header(CONTENT_RANGE, format!("bytes {offset}-{end}/{total}"))
            .header(CONTENT_LENGTH, data.len().to_string())
            .body(data.to_vec())
            .send()
            .await?;

        match resp.status().as_u16() {
            200 | 201 => Ok(total),
            308 => parse_gcs_range_header(&resp),
            status => Err(UploadError::Api {
                status,
                message: format!(
                    "GCS chunk failed: {}",
                    resp.text().await.unwrap_or_default()
                ),
            }),
        }
    }

    async fn query_progress(&self, session: &UploadSession) -> Result<u64, UploadError> {
        let resp = self
            .client
            .put(&session.session_id)
            .header(CONTENT_RANGE, format!("bytes */{}", session.total_size))
            .header(CONTENT_LENGTH, "0")
            .send()
            .await?;

        match resp.status().as_u16() {
            200 | 201 => Ok(session.total_size),
            308 => parse_gcs_range_header(&resp),
            status => Err(UploadError::Api {
                status,
                message: "GCS progress query failed".to_string(),
            }),
        }
    }

    async fn abort_upload(&self, session: &UploadSession) -> Result<(), UploadError> {
        let _ = self.client.delete(&session.session_id).send().await?;
        Ok(())
    }

    /// Upload every part of `file_path` to its presigned PUT URL concurrently
    /// (bounded by `self.part_concurrency`, configured from
    /// `UploadConfig::mpu_part_concurrency`, default [`MPU_PART_CONCURRENCY`]) and
    /// return `(part_number, etag)` for each part, ready to hand to
    /// `complete_multipart_upload`.
    ///
    /// Each part task opens its own `tokio::fs::File`, seeks to
    /// `(part_number - 1) * part_size`, reads exactly its slice (the last part
    /// is the shorter remainder), and PUTs the bytes. On a 200 it reads the
    /// `ETag` response header verbatim. Per-part retry reuses the shared capped
    /// backoff + [`is_retryable`] policy (a part success is a plain 200 + ETag,
    /// never a 308 — `parse_gcs_range_header` is resumable-only).
    ///
    /// `on_progress(confirmed, total)` fires once per part as it lands, summing
    /// confirmed bytes across parts so the existing speed/ETA UI keeps working.
    /// The first error aborts the remaining in-flight tasks and propagates; the
    /// caller is responsible for the best-effort `abort_multipart_upload`.
    #[tracing::instrument(skip_all, fields(
        filename = file_path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
        total_size,
        parts = parts.len(),
        part_size,
    ))]
    pub async fn upload_file_mpu(
        &self,
        file_path: &Path,
        parts: &[PartUrl],
        part_size: u64,
        total_size: u64,
        max_retries: u32,
        on_progress: &ProgressFn,
    ) -> Result<Vec<(u32, String)>, UploadError> {
        // The part layout (per-part offsets/lengths) was computed from
        // `total_size`. If the file is no longer that size, it was mutated after
        // the snapshot (still recording / a cloud-sync placeholder / AV scan) and
        // the parts no longer map to real bytes. Bail with an actionable,
        // non-retryable error before spawning any part PUTs.
        let current_len = tokio::fs::metadata(file_path).await?.len();
        if current_len != total_size {
            return Err(UploadError::FileChangedDuringUpload {
                declared: total_size,
                actual: current_len,
            });
        }
        // `part_concurrency` was clamped to >= 1 at construction, so the
        // semaphore always has at least one permit.
        let semaphore = Arc::new(Semaphore::new(self.part_concurrency));
        let path: PathBuf = file_path.to_path_buf();
        let mut join_set: JoinSet<Result<PartOutcome, UploadError>> = JoinSet::new();

        for part in parts {
            let permit_source = Arc::clone(&semaphore);
            let client = self.client.clone();
            let path = path.clone();
            let part = part.clone();
            let offset = (part.part_number.saturating_sub(1)) as u64 * part_size;
            // Last part is the remainder; clamp so we never read past EOF.
            let len = part_size.min(total_size.saturating_sub(offset));
            join_set.spawn(async move {
                // Hold a permit for the whole part so at most
                // MPU_PART_CONCURRENCY parts are in flight (and in memory) at once.
                let _permit = permit_source.acquire_owned().await.map_err(|_| {
                    UploadError::MpuTaskFailed {
                        part_number: part.part_number as i32,
                        reason: "concurrency semaphore closed".to_string(),
                    }
                })?;
                let etag =
                    put_part_with_retry(&client, &path, &part, offset, len, max_retries).await?;
                Ok(PartOutcome {
                    part_number: part.part_number,
                    etag,
                    bytes: len,
                })
            });
        }

        let mut collected: Vec<(u32, String)> = Vec::with_capacity(parts.len());
        let mut confirmed: u64 = 0;
        while let Some(joined) = join_set.join_next().await {
            let outcome = match joined {
                Ok(result) => result?,
                Err(join_err) => {
                    // A part task panicked or was aborted. Abort the rest and
                    // surface an attributable error.
                    join_set.shutdown().await;
                    return Err(UploadError::MpuTaskFailed {
                        part_number: 0,
                        reason: join_err.to_string(),
                    });
                }
            };
            confirmed = confirmed.saturating_add(outcome.bytes);
            on_progress(confirmed, total_size);
            collected.push((outcome.part_number, outcome.etag));
        }

        // Surface parts in ascending order for stable logs / completion bodies.
        collected.sort_by_key(|(part_number, _)| *part_number);
        Ok(collected)
    }
}

/// Result of one successfully uploaded multipart part.
struct PartOutcome {
    part_number: u32,
    etag: String,
    bytes: u64,
}

/// PUT a single multipart part with the shared capped-backoff retry policy.
/// Returns the verbatim `ETag` response header on success.
async fn put_part_with_retry(
    client: &reqwest::Client,
    file_path: &Path,
    part: &PartUrl,
    offset: u64,
    len: u64,
    max_retries: u32,
) -> Result<String, UploadError> {
    let budget = RetryBudget {
        max_attempts: max_retries,
        max_elapsed: MAX_RETRY_WALL_CLOCK,
    };
    let label = format!("Multipart part {} upload", part.part_number);
    // Probe connectivity through the same (proxy-aware) client the part PUT uses.
    retry_io(
        budget,
        &label,
        || put_part_once(client, file_path, part, offset, len),
        |ms| wait_for_network(client, ms),
    )
    .await
}

/// Read this part's byte slice from `file_path` and PUT it to its signed URL.
/// On a 2xx, return the `ETag` response header verbatim (the value the backend
/// needs to complete the MPU). A missing ETag on an otherwise-successful PUT is
/// a hard error ([`UploadError::MpuMissingEtag`]); a non-2xx maps to
/// [`UploadError::Api`] so [`is_retryable`] can classify 5xx/429/408 as
/// retryable and 4xx as terminal.
async fn put_part_once(
    client: &reqwest::Client,
    file_path: &Path,
    part: &PartUrl,
    offset: u64,
    len: u64,
) -> Result<String, UploadError> {
    let mut file = tokio::fs::File::open(file_path)
        .await
        .map_err(|e| UploadError::from_source_io(e, file_path))?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len as usize];
    // A short read means the file shrank since the part layout was computed —
    // surface it as a file-changed outcome rather than a bare "early eof".
    if let Err(e) = file.read_exact(&mut buf).await {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            let actual = file.metadata().await.map(|m| m.len()).unwrap_or(offset);
            return Err(UploadError::FileChangedDuringUpload {
                declared: offset + len,
                actual,
            });
        }
        return Err(e.into());
    }

    let resp = client
        .put(&part.url)
        .header(CONTENT_LENGTH, buf.len().to_string())
        .body(buf)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        return Err(UploadError::Api {
            status: status.as_u16(),
            message: format!(
                "GCS multipart part {} failed: {}",
                part.part_number,
                resp.text().await.unwrap_or_default()
            ),
        });
    }

    resp.headers()
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or(UploadError::MpuMissingEtag {
            part_number: part.part_number as i32,
        })
}

fn parse_gcs_range_header(resp: &reqwest::Response) -> Result<u64, UploadError> {
    resp.headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("bytes=0-"))
        .and_then(|end| end.parse::<u64>().ok())
        .map(|end| end + 1)
        .ok_or_else(|| UploadError::Api {
            status: 500,
            message: "GCS did not return valid Range header".to_string(),
        })
}

// ── S3-Compatible Backend ──────────────────────────────────────────────

/// S3-compatible multipart upload backend.
/// Works with AWS S3, Alibaba OSS, Tencent COS, MinIO, etc.
pub struct S3Backend {
    client: reqwest::Client,
    #[allow(dead_code)]
    endpoint: String,
    #[allow(dead_code)]
    access_key: String,
    #[allow(dead_code)]
    secret_key: String,
    #[allow(dead_code)]
    region: String,
}

impl S3Backend {
    pub fn new(endpoint: String, access_key: String, secret_key: String, region: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint,
            access_key,
            secret_key,
            region,
        }
    }

    async fn initiate_upload(
        &self,
        signed_url: &str,
        content_type: &str,
        total_size: u64,
    ) -> Result<UploadSession, UploadError> {
        let url = append_query(signed_url, "uploads");

        let resp = self
            .client
            .post(&url)
            .header(CONTENT_TYPE, content_type)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: format!(
                    "S3 initiate failed: {}",
                    resp.text().await.unwrap_or_default()
                ),
            });
        }

        let body = resp.text().await.unwrap_or_default();
        let upload_id = extract_xml_value(&body, "UploadId").ok_or_else(|| UploadError::Api {
            status: 500,
            message: "S3 did not return UploadId".to_string(),
        })?;

        Ok(UploadSession {
            session_id: format!("{signed_url}|{upload_id}"),
            total_size,
            bytes_confirmed: 0,
        })
    }

    async fn upload_chunk(
        &self,
        session: &UploadSession,
        data: &[u8],
        offset: u64,
    ) -> Result<u64, UploadError> {
        let (base_url, upload_id) = parse_s3_session_id(&session.session_id)?;
        let part_number = (offset / DEFAULT_CHUNK_SIZE.max(1)) as u32 + 1;
        let url = format!(
            "{}{sep}partNumber={part_number}&uploadId={upload_id}",
            base_url,
            sep = if base_url.contains('?') { "&" } else { "?" },
        );

        let resp = self
            .client
            .put(&url)
            .header(CONTENT_LENGTH, data.len().to_string())
            .body(data.to_vec())
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: format!("S3 upload part {part_number} failed"),
            });
        }

        Ok(offset + data.len() as u64)
    }

    async fn query_progress(&self, session: &UploadSession) -> Result<u64, UploadError> {
        let (base_url, upload_id) = parse_s3_session_id(&session.session_id)?;
        let url = format!(
            "{}{sep}uploadId={upload_id}",
            base_url,
            sep = if base_url.contains('?') { "&" } else { "?" },
        );

        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: "S3 list parts failed".to_string(),
            });
        }

        let body = resp.text().await.unwrap_or_default();
        let total_bytes: u64 = extract_all_xml_values(&body, "Size")
            .iter()
            .filter_map(|s| s.parse::<u64>().ok())
            .sum();
        Ok(total_bytes)
    }

    async fn abort_upload(&self, session: &UploadSession) -> Result<(), UploadError> {
        let (base_url, upload_id) = parse_s3_session_id(&session.session_id)?;
        let url = format!(
            "{}{sep}uploadId={upload_id}",
            base_url,
            sep = if base_url.contains('?') { "&" } else { "?" },
        );
        let _ = self.client.delete(&url).send().await?;
        Ok(())
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn append_query(url: &str, param: &str) -> String {
    if url.contains('?') {
        format!("{url}&{param}")
    } else {
        format!("{url}?{param}")
    }
}

fn parse_s3_session_id(session_id: &str) -> Result<(&str, &str), UploadError> {
    session_id.split_once('|').ok_or_else(|| UploadError::Api {
        status: 500,
        message: "Invalid S3 session ID format".to_string(),
    })
}

fn extract_xml_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

fn extract_all_xml_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(start_pos) = xml[search_from..].find(&open) {
        let abs_start = search_from + start_pos + open.len();
        if let Some(end_pos) = xml[abs_start..].find(&close) {
            results.push(xml[abs_start..abs_start + end_pos].to_string());
            search_from = abs_start + end_pos + close.len();
        } else {
            break;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    /// The shipped `config.chunk_size_mb` default (8), in bytes.
    const DEFAULT_FLOOR: u64 = 8 * MIB;

    #[test]
    fn chunk_size_is_always_256kib_aligned() {
        for size in [1_u64, 1024, 50 * MIB, 1331 * MIB, 3 * GIB, 10 * GIB] {
            let chunk = pick_chunk_size(size, DEFAULT_FLOOR);
            assert_eq!(
                chunk % CHUNK_QUANTUM,
                0,
                "chunk {chunk} for size {size} not 256 KiB aligned"
            );
        }
    }

    #[test]
    fn small_and_medium_files_use_the_floor() {
        assert_eq!(
            pick_chunk_size(50 * MIB, DEFAULT_FLOOR),
            MIN_AUTO_CHUNK_SIZE
        );
        assert_eq!(
            pick_chunk_size(200 * MIB, DEFAULT_FLOOR),
            MIN_AUTO_CHUNK_SIZE
        );
    }

    #[test]
    fn large_files_scale_up_but_stay_capped() {
        // ~1.24 GiB lands between the floor and the cap.
        let chunk = pick_chunk_size(1331 * MIB, DEFAULT_FLOOR);
        assert!(
            chunk > MIN_AUTO_CHUNK_SIZE && chunk <= MAX_AUTO_CHUNK_SIZE,
            "got {chunk}"
        );
        // Multi-GB files saturate at the cap.
        assert_eq!(pick_chunk_size(3 * GIB, DEFAULT_FLOOR), MAX_AUTO_CHUNK_SIZE);
        assert_eq!(
            pick_chunk_size(10 * GIB, DEFAULT_FLOOR),
            MAX_AUTO_CHUNK_SIZE
        );
    }

    #[test]
    fn chunk_count_stays_bounded_for_large_files() {
        let size = 1331 * MIB;
        let chunks = size.div_ceil(pick_chunk_size(size, DEFAULT_FLOOR));
        assert!(
            (20..=64).contains(&chunks),
            "expected tens of chunks, got {chunks}"
        );
    }

    #[test]
    fn explicit_large_floor_overrides_the_cap() {
        // chunk_size_mb = 128 -> the floor wins over the 64 MiB auto cap.
        assert_eq!(pick_chunk_size(3 * GIB, 128 * MIB), 128 * MIB);
    }

    #[test]
    fn zero_floor_falls_back_to_min() {
        assert_eq!(pick_chunk_size(50 * MIB, 0), MIN_AUTO_CHUNK_SIZE);
    }

    #[test]
    fn retry_budget_stops_when_attempts_exhausted() {
        let budget = RetryBudget {
            max_attempts: 5,
            max_elapsed: std::time::Duration::from_secs(600),
        };
        // All 5 attempts already spent, barely any time elapsed: no more retries.
        assert!(!budget.allows_retry(5, std::time::Duration::from_secs(1)));
        // 4 spent, plenty of budget left on both axes: keep going.
        assert!(budget.allows_retry(4, std::time::Duration::from_secs(1)));
    }

    #[test]
    fn retry_budget_stops_when_wall_clock_exhausted() {
        // The real fix: even with attempts to spare, a chunk that has been
        // grinding past the wall-clock cap must give up instead of retrying
        // for hours (each stalled attempt can burn the full request timeout).
        let budget = RetryBudget {
            max_attempts: 100,
            max_elapsed: std::time::Duration::from_secs(600),
        };
        // Only 3 attempts made, but 11 minutes have elapsed: stop.
        assert!(!budget.allows_retry(3, std::time::Duration::from_secs(660)));
        // 3 attempts, 9 minutes: still within the wall-clock budget.
        assert!(budget.allows_retry(3, std::time::Duration::from_secs(540)));
    }

    /// A retryable error that is trivial to construct in tests (a 503 is
    /// classified retryable by `is_retryable`).
    fn transient() -> UploadError {
        UploadError::Api {
            status: 503,
            message: "stall".to_string(),
        }
    }

    #[tokio::test]
    async fn retry_io_gives_up_after_the_attempt_budget() {
        let budget = RetryBudget {
            max_attempts: 3,
            max_elapsed: std::time::Duration::from_secs(600),
        };
        let calls = std::cell::Cell::new(0u32);
        // Sleep is injected as a no-op so the test neither waits nor touches the
        // network (the real callers pass `wait_for_network`).
        let result: Result<u64, UploadError> = retry_io(
            budget,
            "test",
            || {
                calls.set(calls.get() + 1);
                async { Err(transient()) }
            },
            |_delay_ms| async {},
        )
        .await;
        assert!(result.is_err());
        // 1 initial attempt + 3 retries, then the budget stops it.
        assert_eq!(calls.get(), 4);
    }

    #[tokio::test]
    async fn retry_io_succeeds_after_transient_failures() {
        let budget = RetryBudget {
            max_attempts: 5,
            max_elapsed: std::time::Duration::from_secs(600),
        };
        let calls = std::cell::Cell::new(0u32);
        let result: Result<u64, UploadError> = retry_io(
            budget,
            "test",
            || {
                let n = calls.get() + 1;
                calls.set(n);
                // Compute the outcome here so the async block stays flat (the
                // first two attempts fail transiently, the third succeeds).
                let outcome = if n < 3 { Err(transient()) } else { Ok(42) };
                async move { outcome }
            },
            |_delay_ms| async {},
        )
        .await;
        assert_eq!(result.expect("should succeed on the 3rd attempt"), 42);
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn retry_io_does_not_retry_a_terminal_error() {
        let budget = RetryBudget {
            max_attempts: 100,
            max_elapsed: std::time::Duration::from_secs(600),
        };
        let calls = std::cell::Cell::new(0u32);
        // A 404 is NOT retryable: the loop must surface it on the first failure.
        let result: Result<u64, UploadError> = retry_io(
            budget,
            "test",
            || {
                calls.set(calls.get() + 1);
                async {
                    Err(UploadError::Api {
                        status: 404,
                        message: "gone".to_string(),
                    })
                }
            },
            |_delay_ms| async {},
        )
        .await;
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }
}
