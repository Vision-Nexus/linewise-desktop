use crate::auth::AuthService;
use crate::config::Environment;
use crate::error::UploadError;
use crate::models::{
    CreateDocumentRequest, DedupCheckRequest, DedupCheckResponse, DocumentResponse,
    PresignedUrlResponse, Project, QualityCheckResponse, WhoAmIResponse,
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
    pub async fn whoami(&self) -> Result<WhoAmIResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .get(format!("{}/api/users/whoami", self.base_url))
            .headers(headers)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }

        Ok(resp.json().await?)
    }

    /// GET /api/org/{tenant}/projects
    pub async fn list_projects(&self, tenant: &str) -> Result<Vec<Project>, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .get(format!("{}/api/org/{}/projects", self.base_url, tenant))
            .headers(headers)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }

        Ok(resp.json().await?)
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents
    pub async fn create_document(
        &self,
        tenant: &str,
        project_id: &str,
        request: &CreateDocumentRequest,
    ) -> Result<DocumentResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .post(format!(
                "{}/api/org/{}/projects/{}/documents",
                self.base_url, tenant, project_id
            ))
            .headers(headers)
            .json(request)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }

        Ok(resp.json().await?)
    }

    /// POST /api/org/{tenant}/projects/{pid}/documents/{did}/upload-url?resumable=true
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

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }

        Ok(resp.json().await?)
    }

    /// GET /api/org/{tenant}/projects/{pid}/documents/{did}
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

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }

        Ok(resp.json().await?)
    }

    /// POST /api/org/{tenant}/dedup-checks
    ///
    /// Batch query against the cross-tenant MD5 dedup registry. Each
    /// hash is a 32-char lowercase hex string. The server caps the
    /// batch at 100 (returns 400 above that) and at minimum 1
    /// (returns 400 on empty). Results are not guaranteed to come
    /// back in request order — callers must correlate by `md5_hash`.
    pub async fn check_dedup(
        &self,
        tenant: &str,
        md5_hashes: &[String],
    ) -> Result<DedupCheckResponse, UploadError> {
        let headers = self.auth_headers().await?;
        let resp = self
            .client
            .post(format!("{}/api/org/{}/dedup-checks", self.base_url, tenant))
            .headers(headers)
            .json(&DedupCheckRequest { md5_hashes })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }

        Ok(resp.json().await?)
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
    pub async fn quality_check(
        &self,
        tenant: &str,
        project_id: &str,
        atoms: AtomChunks,
    ) -> Result<QualityCheckResponse, UploadError> {
        let payload_bytes = atoms.payload_bytes();
        if payload_bytes > MAX_PAYLOAD_BYTES {
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

        let mut headers = self.auth_headers().await?;
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
            .post(format!(
                "{}/api/org/{}/projects/{}/quality-check",
                self.base_url, tenant, project_id,
            ))
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(quality_check_send_error)?;

        if !resp.status().is_success() {
            return Err(UploadError::Api {
                status: resp.status().as_u16(),
                message: resp.text().await.unwrap_or_default(),
            });
        }

        Ok(resp.json().await?)
    }

    /// Verify document upload by polling until gcsUri is set
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
                return Ok(doc);
            }
            if i < max_retries - 1 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }

        Err(UploadError::Api {
            status: 408,
            message: "Upload verification timed out".to_string(),
        })
    }
}

/// Map a `reqwest::Error` from `quality_check` to either
/// `QualityCheckOffline` (connect / timeout — server unreachable) or
/// the generic `Network` variant. The classifier matters because the
/// hard cutover means an offline launch can't run a local rule check
/// any more, and the UI wants to render that dedicated message.
fn quality_check_send_error(err: reqwest::Error) -> UploadError {
    if err.is_connect() || err.is_timeout() {
        UploadError::QualityCheckOffline { source: err }
    } else {
        UploadError::Network(err)
    }
}
