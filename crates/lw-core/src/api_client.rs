use crate::auth::AuthService;
use crate::config::Environment;
use crate::error::UploadError;
use crate::models::{
    CreateDocumentRequest, DigestCheckCandidate, DigestCheckRequest, DigestCheckResponse,
    DocumentResponse, PresignedUrlResponse, Project, QualityCheckResponse, WhoAmIResponse,
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
    pub fn new(environment: Environment, auth: Arc<AuthService>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: environment.api_base_url().to_string(),
            auth,
        }
    }

    async fn auth_headers(&self) -> Result<HeaderMap, UploadError> {
        let token = self
            .auth
            .get_id_token()
            .await
            .map_err(|e| UploadError::Api {
                status: 401,
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
