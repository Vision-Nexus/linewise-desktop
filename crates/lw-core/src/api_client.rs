use crate::auth::AuthService;
use crate::config::Environment;
use crate::error::UploadError;
use crate::models::{
    CreateDocumentRequest, DigestCheckCandidate, DigestCheckRequest, DigestCheckResponse,
    DocumentResponse, MultipartAbortRequest, MultipartCompletePart, MultipartCompleteRequest,
    MultipartPlan, MultipartResumePlan, MultipartResumeRequest, PresignedUrlResponse, Project,
    QualityCheckResponse, WhoAmIResponse,
};
use crate::video_head::{AtomChunks, MAX_PAYLOAD_BYTES};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use std::sync::Arc;

pub struct ApiClient {
    client: reqwest::Client,
    base_url: String,
    auth: Arc<AuthService>,
}

impl ApiClient {
    /// Build the Linewise API client. `proxy` is the optional fixed proxy
    /// URL from config (`ServerConfig::proxy_url`); empty/`None` keeps the
    /// historical no-explicit-proxy behaviour. A 120s total timeout covers
    /// the largest call (the quality-check head-byte POST, capped at 16 MiB)
    /// on a slow link; the 10s connect timeout fails a dead/wrong proxy fast.
    pub fn new(environment: Environment, auth: Arc<AuthService>, proxy: Option<&str>) -> Self {
        let client = crate::net::build_http_client(
            proxy,
            Some(std::time::Duration::from_secs(120)),
            std::time::Duration::from_secs(10),
        )
        .expect("failed to build reqwest client");
        Self {
            client,
            base_url: environment.api_base_url().to_string(),
            auth,
        }
    }

    async fn auth_headers(&self) -> Result<HeaderMap, UploadError> {
        let token = self
            .auth
            .get_id_token()
            .await
            // A token-fetch failure is an auth/session problem, not an HTTP 401
            // from our API — most often the Firebase token refresh couldn't reach
            // securetoken (a transport failure). Surface the real message via
            // UploadError::Auth instead of mislabelling it "API error (401)".
            .map_err(|e| UploadError::Auth {
                message: e.to_string(),
            })?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid header"),
        );
        Ok(headers)
    }

    /// GET /api/users/whoami
    #[tracing::instrument(skip_all)]
    pub async fn whoami(&self) -> Result<WhoAmIResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .get(format!("{}/api/users/whoami", self.base_url))
            .headers(headers)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "whoami non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        Ok(resp.json().await?)
    }

    /// GET /api/org/{tenant}/projects
    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    pub async fn list_projects(&self, tenant: &str) -> Result<Vec<Project>, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .get(format!("{}/api/org/{}/projects", self.base_url, tenant))
            .headers(headers)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "list_projects non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let projects: Vec<Project> = resp.json().await?;
        tracing::info!(count = projects.len(), "list_projects ok");
        Ok(projects)
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents
    #[tracing::instrument(skip_all, fields(tenant = %tenant, project_id = %project_id))]
    pub async fn create_document(
        &self,
        tenant: &str,
        project_id: &str,
        request: &CreateDocumentRequest,
    ) -> Result<DocumentResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let url = format!(
            "{}/api/org/{}/projects/{}/documents",
            self.base_url, tenant, project_id
        );
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(request)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "create_document non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let doc: DocumentResponse = resp.json().await?;
        tracing::info!(document_id = %doc.id, "create_document ok");
        Ok(doc)
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents/{did}/upload-url?resumable=true
    #[tracing::instrument(skip_all, fields(tenant = %tenant, project_id = %project_id, document_id = %document_id))]
    pub async fn get_upload_url(
        &self,
        tenant: &str,
        project_id: &str,
        document_id: &str,
    ) -> Result<PresignedUrlResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .post(format!(
                "{}/api/org/{}/projects/{}/documents/{}/upload-url?resumable=true",
                self.base_url, tenant, project_id, document_id
            ))
            .headers(headers)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "get_upload_url non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        tracing::debug!("upload-url issued");
        Ok(resp.json().await?)
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents/{did}/multipart-upload-url
    ///
    /// Ask the backend to initiate a GCS XML Multipart Upload and presign one
    /// PUT URL per part. Returns the [`MultipartPlan`] (uploadId + partSize +
    /// presigned parts) on success.
    ///
    /// Returns `Ok(None)` when the backend answers **404 Not Found** — i.e.
    /// this is a server build that predates the multipart feature. The caller
    /// uses that as the signal to fall back to the existing resumable path
    /// (safe rollout). Any other non-2xx is a real failure and surfaces as
    /// [`UploadError::Api`].
    #[tracing::instrument(skip_all, fields(tenant = %tenant, project_id = %project_id, document_id = %document_id, total_size))]
    pub async fn get_multipart_upload(
        &self,
        tenant: &str,
        project_id: &str,
        document_id: &str,
        total_size: i64,
    ) -> Result<Option<MultipartPlan>, UploadError> {
        let headers = self.auth_headers().await?;
        let url = format!(
            "{}/api/org/{}/projects/{}/documents/{}/multipart-upload-url",
            self.base_url, tenant, project_id, document_id
        );
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(&serde_json::json!({ "totalSize": total_size }))
            .send()
            .await?;

        let status = resp.status();
        // 404 = backend without the multipart feature → signal fallback.
        if status.as_u16() == 404 {
            tracing::info!("multipart-upload-url 404 — backend lacks MPU, falling back");
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "get_multipart_upload non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let plan: MultipartPlan = resp.json().await?;
        tracing::info!(
            upload_id = %plan.upload_id,
            part_size = plan.part_size,
            parts = plan.parts.len(),
            "multipart-upload-url issued"
        );
        Ok(Some(plan))
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents/{did}/multipart-upload-complete
    ///
    /// Finalize the MPU: hand the backend every part's `(partNumber, etag)`
    /// so it can assemble the object. `parts` is the driver's collected
    /// `(part_number, etag)` set; ordering does not matter (the request
    /// carries the explicit `partNumber`), but the backend expects every part.
    #[tracing::instrument(skip_all, fields(tenant = %tenant, project_id = %project_id, document_id = %document_id, parts = parts.len()))]
    pub async fn complete_multipart_upload(
        &self,
        tenant: &str,
        project_id: &str,
        document_id: &str,
        upload_id: &str,
        parts: &[(u32, String)],
    ) -> Result<(), UploadError> {
        let headers = self.auth_headers().await?;
        let url = format!(
            "{}/api/org/{}/projects/{}/documents/{}/multipart-upload-complete",
            self.base_url, tenant, project_id, document_id
        );
        let body = MultipartCompleteRequest {
            upload_id: upload_id.to_string(),
            parts: parts
                .iter()
                .map(|(part_number, etag)| MultipartCompletePart {
                    part_number: *part_number as i32,
                    etag: etag.clone(),
                })
                .collect(),
        };
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "complete_multipart_upload non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        tracing::info!("multipart-upload-complete ok");
        Ok(())
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents/{did}/multipart-upload-abort
    ///
    /// Best-effort cancellation of an initiated MPU after a terminal part
    /// failure or a user cancel. The caller treats the result as advisory:
    /// the goal is to release the server-side MPU so orphaned parts don't
    /// linger, but a failed abort must not mask the original error.
    #[tracing::instrument(skip_all, fields(tenant = %tenant, project_id = %project_id, document_id = %document_id))]
    pub async fn abort_multipart_upload(
        &self,
        tenant: &str,
        project_id: &str,
        document_id: &str,
        upload_id: &str,
    ) -> Result<(), UploadError> {
        let headers = self.auth_headers().await?;
        let url = format!(
            "{}/api/org/{}/projects/{}/documents/{}/multipart-upload-abort",
            self.base_url, tenant, project_id, document_id
        );
        let body = MultipartAbortRequest {
            upload_id: upload_id.to_string(),
        };
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "multipart-upload-abort non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        tracing::info!("multipart-upload-abort ok");
        Ok(())
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents/{did}/multipart-upload-resume
    ///
    /// Resume a persisted MPU after an app restart. The backend runs GCS
    /// ListParts against `upload_id` and returns the parts still missing (with
    /// fresh signed PUT URLs) plus the parts already durable on GCS (with their
    /// server-reported ETags). `total_size` MUST equal the original initiate's
    /// size so the backend re-derives the identical part layout.
    ///
    /// `Ok(None)` is the fallback signal, returned on **404** — which covers
    /// both "this backend has no resume endpoint" and "GCS reports the upload
    /// no longer exists" (`NoSuchUpload`, i.e. expired or already completed).
    /// Either way the caller abandons the stale id and starts a fresh MPU.
    #[tracing::instrument(skip_all, fields(tenant = %tenant, project_id = %project_id, document_id = %document_id, total_size))]
    pub async fn resume_multipart_upload(
        &self,
        tenant: &str,
        project_id: &str,
        document_id: &str,
        upload_id: &str,
        total_size: i64,
    ) -> Result<Option<MultipartResumePlan>, UploadError> {
        let headers = self.auth_headers().await?;
        let url = format!(
            "{}/api/org/{}/projects/{}/documents/{}/multipart-upload-resume",
            self.base_url, tenant, project_id, document_id
        );
        let body = MultipartResumeRequest {
            upload_id: upload_id.to_string(),
            total_size,
        };
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        // 404 = no resume endpoint (old backend) OR NoSuchUpload (stale /
        // already-completed) → fall back to a fresh MPU.
        if status.as_u16() == 404 {
            tracing::info!("multipart-upload-resume 404 — abandoning stale uploadId, fresh MPU");
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "resume_multipart_upload non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let plan: MultipartResumePlan = resp.json().await?;
        tracing::info!(
            upload_id = %plan.upload_id,
            part_size = plan.part_size,
            remaining = plan.parts.len(),
            completed = plan.completed_parts.len(),
            "multipart-upload-resume issued"
        );
        Ok(Some(plan))
    }

    /// GET /api/org/{tenant}/projects/{pid}/documents/{did}
    #[tracing::instrument(skip_all, fields(tenant = %tenant, project_id = %project_id, document_id = %document_id))]
    pub async fn get_document(
        &self,
        tenant: &str,
        project_id: &str,
        document_id: &str,
    ) -> Result<DocumentResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .get(format!(
                "{}/api/org/{}/projects/{}/documents/{}",
                self.base_url, tenant, project_id, document_id
            ))
            .headers(headers)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "get_document non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        Ok(resp.json().await?)
    }

    /// POST /api/org/{tenant}/digest-checks
    ///
    /// Multi-signal (V2) cross-tenant dedup query. Each candidate carries
    /// any subset of `{md5, crc32c, sha256_head_256kib}`; the desktop
    /// sends one candidate with all three legs of the file's digest. The
    /// server matches on the verified `(crc32c, sha256_head_256kib)` pair
    /// in addition to md5, so a file uploaded via a resumable path (GCS
    /// exposes crc32c but no md5) is still found — which the legacy
    /// md5-only `/dedup-checks` could not do.
    ///
    /// The legacy `/dedup-checks` route is deliberately left in place on
    /// the server for older desktop builds; this client no longer calls
    /// it.
    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    pub async fn check_digests(
        &self,
        tenant: &str,
        candidate: &DigestCheckCandidate,
    ) -> Result<DigestCheckResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let url = format!("{}/api/org/{}/digest-checks", self.base_url, tenant);
        let body = DigestCheckRequest {
            candidates: std::slice::from_ref(candidate),
        };
        let resp = self
            .client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "check_digests non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        let parsed: DigestCheckResponse = resp.json().await?;
        tracing::debug!(results = parsed.results.len(), "digest-checks ok");
        Ok(parsed)
    }

    /// POST /api/org/{tenant}/projects/{pid}/quality-check
    ///
    /// Ship the sparse atom layout assembled by
    /// [`crate::video_head::extract_atom_chunks`] and let the server run
    /// `ffprobe` against a reconstructed sparse temp file, then apply
    /// the global+per-project rule set. The body is the concatenated
    /// chunk bytes; `X-Linewise-Atom-Layout` carries the
    /// `<offset>:<len>,...` map so the server knows where each chunk
    /// sits in the original file.
    ///
    /// Errors:
    ///   * [`UploadError::QualityCheckPayloadTooLarge`] if the assembled
    ///     payload exceeds [`MAX_PAYLOAD_BYTES`] — refused before the
    ///     network call so we don't burn bandwidth.
    ///   * [`UploadError::QualityCheckOffline`] when the request fails
    ///     due to connect / timeout / DNS — distinct from
    ///     [`UploadError::Api`] so the UI can render the dedicated
    ///     "server unreachable" message after the hard cutover.
    ///   * [`UploadError::Api`] for non-2xx responses with the body as
    ///     the message.
    #[tracing::instrument(skip_all, fields(
        tenant = %tenant,
        project_id = %project_id,
        payload_bytes = atoms.payload_bytes(),
        total_size = atoms.total_size,
    ))]
    pub async fn quality_check(
        &self,
        tenant: &str,
        project_id: &str,
        atoms: AtomChunks,
    ) -> Result<QualityCheckResponse, UploadError> {
        let url = format!(
            "{}/api/org/{}/projects/{}/quality-check",
            self.base_url, tenant, project_id,
        );
        let headers = self.auth_headers().await?;
        self.post_quality_check(&url, headers, atoms, "quality_check")
            .await
    }

    /// POST /api/public/quality-check
    ///
    /// Unauthenticated sibling of [`Self::quality_check`] for the
    /// pre-login playground tab (VLP-545). Vendors drag a clip in
    /// before signing in, so there's no Firebase token, no tenant,
    /// and no project context to resolve per-project rule overrides
    /// against. The server always evaluates against
    /// `VideoQualityDefaultRules.defaultRules`.
    ///
    /// The wire format is identical to the authed path — same atom
    /// layout header, same body shape, same [`QualityCheckResponse`]
    /// JSON — so the playground UI can share verdict-rendering code
    /// with the upload-time gate.
    ///
    /// Errors mirror [`Self::quality_check`]: payload-too-large is
    /// refused locally, transport failures classify as
    /// [`UploadError::QualityCheckOffline`], non-2xx responses become
    /// [`UploadError::Api`].
    #[tracing::instrument(skip_all, fields(
        payload_bytes = atoms.payload_bytes(),
        total_size = atoms.total_size,
    ))]
    pub async fn quality_check_public(
        &self,
        atoms: AtomChunks,
    ) -> Result<QualityCheckResponse, UploadError> {
        let url = format!("{}/api/public/quality-check", self.base_url);
        self.post_quality_check(&url, HeaderMap::new(), atoms, "quality_check_public")
            .await
    }

    /// Body shared by [`Self::quality_check`] and
    /// [`Self::quality_check_public`]: enforce the local payload cap,
    /// build the layout header + concatenated body, attach the
    /// caller-provided base headers (auth or empty), POST, and decode.
    /// `op_label` is a short identifier baked into log messages so
    /// the two call sites stay distinguishable in traces.
    async fn post_quality_check(
        &self,
        url: &str,
        mut headers: HeaderMap,
        atoms: AtomChunks,
        op_label: &'static str,
    ) -> Result<QualityCheckResponse, UploadError> {
        let payload_bytes = atoms.payload_bytes();
        if payload_bytes > MAX_PAYLOAD_BYTES {
            tracing::warn!(
                bytes = payload_bytes,
                cap = MAX_PAYLOAD_BYTES,
                "{op_label} payload over cap"
            );
            return Err(UploadError::QualityCheckPayloadTooLarge {
                bytes: payload_bytes,
                cap: MAX_PAYLOAD_BYTES,
            });
        }

        let layout = atoms
            .chunks
            .iter()
            .map(|(off, b)| format!("{off}:{}", b.len()))
            .collect::<Vec<_>>()
            .join(",");
        let total_size = atoms.total_size;
        let body: Vec<u8> = atoms.chunks.into_iter().flat_map(|(_, b)| b).collect();

        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(
            "X-Linewise-Total-Size",
            HeaderValue::from_str(&total_size.to_string())
                .expect("decimal digits are valid header bytes"),
        );
        headers.insert(
            "X-Linewise-Atom-Layout",
            HeaderValue::from_str(&layout)
                .expect("ascii digits/colons/commas are valid header bytes"),
        );

        let resp = self
            .client
            .post(url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(quality_check_send_error)?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(
                status = status.as_u16(),
                body = truncate(&body, 256),
                "{op_label} non-2xx"
            );
            return Err(UploadError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        Ok(resp.json().await?)
    }

    /// Verify document upload by polling until gcsUri is set
    #[tracing::instrument(skip_all, fields(
        tenant = %tenant,
        project_id = %project_id,
        document_id = %document_id,
        max_retries,
    ))]
    pub async fn verify_upload(
        &self,
        tenant: &str,
        project_id: &str,
        document_id: &str,
        max_retries: u32,
    ) -> Result<DocumentResponse, UploadError> {
        for i in 0..max_retries {
            let doc = self.get_document(tenant, project_id, document_id).await?;
            if doc.gcs_uri.is_some() {
                tracing::info!(attempt = i, "verify_upload ok");
                return Ok(doc);
            }
            if i < max_retries - 1 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }

        tracing::warn!("verify_upload timed out");
        Err(UploadError::Api {
            status: 408,
            message: "Upload verification timed out".to_string(),
        })
    }
}

/// Truncate a string at a byte boundary for safe logging of response bodies.
/// Avoids the panic-on-non-char-boundary of `&s[..n]` when the body is UTF-8
/// with multi-byte characters.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Map a `reqwest::Error` from `quality_check` to either
/// `QualityCheckOffline` (server unreachable from the client side) or the
/// generic `Network` variant. The classifier matters because the hard cutover
/// means an offline launch can't run a local rule check any more, and the UI
/// wants to render that dedicated message.
///
/// Offline covers everything that fails *before* an HTTP response arrives:
/// connect failures (DNS, refused, network unreachable, TLS handshake — all
/// classified as `is_connect()` by reqwest's connector), TCP/TLS timeouts,
/// and request-builder failures (URL parse, header build). A response with a
/// non-2xx status is *not* offline — that surfaces as `UploadError::Api` from
/// the response-handling path, never reaches this classifier.
fn quality_check_send_error(err: reqwest::Error) -> UploadError {
    if err.is_connect() || err.is_timeout() || err.is_request() {
        tracing::warn!(?err, "quality_check offline");
        UploadError::QualityCheckOffline { source: err }
    } else {
        tracing::warn!(?err, "quality_check network error");
        UploadError::Network(err)
    }
}
