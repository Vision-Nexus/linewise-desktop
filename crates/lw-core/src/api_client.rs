use crate::auth::AuthService;
use crate::config::Environment;
use crate::error::UploadError;
use crate::models::{
    CreateDocumentRequest, DocumentResponse, PresignedUrlResponse, Project, WhoAmIResponse,
};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
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
