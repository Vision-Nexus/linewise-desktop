//! Cloud-agnostic resumable upload abstraction.
//!
//! Two backends: GCS (via signed URLs from Linewise API) and S3-compatible
//! (covers AWS S3, Alibaba OSS, Tencent COS, MinIO, etc.).

use crate::error::UploadError;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Default chunk size: 32 MiB (must be multiple of 256 KiB for GCS).
/// Larger chunks = fewer requests = faster for big video files.
const DEFAULT_CHUNK_SIZE: u64 = 32 * 1024 * 1024;

/// Handle to an in-progress resumable upload session, persisted in SQLite
#[derive(Debug, Clone)]
pub struct UploadSession {
    pub session_id: String,
    pub total_size: u64,
    pub bytes_confirmed: u64,
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
}

/// Upload a file with chunked resumable protocol.
pub async fn upload_file_chunked(
    backend: &StorageBackend,
    session: &UploadSession,
    file_path: &Path,
    start_offset: u64,
    chunk_size: u64,
    on_progress: &ProgressFn,
) -> Result<u64, UploadError> {
    let mut file = tokio::fs::File::open(file_path).await?;
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

    while offset < total {
        let this_chunk = (total - offset).min(chunk_size) as usize;
        let mut buf = vec![0u8; this_chunk];
        file.read_exact(&mut buf).await?;

        let confirmed = upload_chunk_with_retry(backend, session, &buf, offset).await?;
        offset = confirmed;
        on_progress(offset, total);
    }

    Ok(offset)
}

/// Upload a single chunk with automatic retry on network errors.
/// Uses exponential backoff: 1s → 2s → 4s → 8s → 16s (max 5 retries).
/// Only retries on network/timeout errors, not on 4xx API errors.
async fn upload_chunk_with_retry(
    backend: &StorageBackend,
    session: &UploadSession,
    data: &[u8],
    offset: u64,
) -> Result<u64, UploadError> {
    const MAX_RETRIES: u32 = 5;
    const INITIAL_DELAY_MS: u64 = 1000;

    let mut attempt = 0;
    loop {
        match backend.upload_chunk(session, data, offset).await {
            Ok(confirmed) => return Ok(confirmed),
            Err(e) if is_retryable(&e) && attempt < MAX_RETRIES => {
                attempt += 1;
                let delay = INITIAL_DELAY_MS * 2u64.pow(attempt - 1);
                tracing::warn!(
                    "Chunk upload failed (attempt {attempt}/{MAX_RETRIES}), retrying in {delay}ms: {e}"
                );
                // Wait for network recovery — poll connectivity before sleeping
                wait_for_network(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
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
        _ => false,
    }
}

/// Wait for network recovery with exponential backoff.
/// Checks connectivity by attempting a lightweight request.
async fn wait_for_network(max_wait_ms: u64) {
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
        match reqwest::Client::new()
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
}

impl Default for GcsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl GcsBackend {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build reqwest client"),
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
