pub mod claims;
mod oauth;

use crate::error::AuthError;
use crate::models::AuthTokens;
use chrono::{Duration, Utc};
use oauth::OAuthProvider;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

const FIREBASE_AUTH_URL: &str = "https://identitytoolkit.googleapis.com/v1";
const FIREBASE_TOKEN_URL: &str = "https://securetoken.googleapis.com/v1/token";
const KEYRING_SERVICE: &str = "linewise-desktop";
const KEYRING_USER: &str = "refresh_token";

/// Client-side auth configuration: Firebase project key plus OAuth provider
/// client IDs. Bundled into one struct so `AuthService::new` has a single
/// parameter and the config layer can eventually swap env-specific values.
///
/// `google_oauth_client_secret` is the secret Google issues alongside a
/// Desktop-app OAuth client. Google's docs call it "not confidential" —
/// it ships in the binary — but Google's token endpoint still requires it
/// on Desktop clients even when PKCE is used. Microsoft native-app clients
/// have no such requirement, so no Microsoft secret field is needed.
#[derive(Clone, Debug)]
pub struct AuthClientConfig {
    pub firebase_api_key: String,
    pub google_oauth_client_id: String,
    pub google_oauth_client_secret: String,
    pub microsoft_oauth_client_id: String,
}

/// Firebase Auth REST API response for sign-in
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignInResponse {
    id_token: String,
    refresh_token: String,
    expires_in: String,
    #[allow(dead_code)]
    local_id: String,
    #[allow(dead_code)]
    email: String,
    #[allow(dead_code)]
    display_name: Option<String>,
}

/// Firebase Auth error response
#[derive(Debug, Deserialize)]
struct FirebaseErrorResponse {
    error: FirebaseError,
}

#[derive(Debug, Deserialize)]
struct FirebaseError {
    code: u16,
    message: String,
}

/// Token refresh response
#[derive(Debug, Deserialize)]
struct RefreshResponse {
    id_token: String,
    refresh_token: String,
    expires_in: String,
}

/// Whether a token-refresh failure is worth retrying. Transport errors
/// (couldn't reach `securetoken.googleapis.com` — DNS/TLS/connection/proxy) and
/// server-side 5xx/429 are transient; a Firebase 4xx (e.g. invalid or revoked
/// refresh token) is terminal and must not be retried.
fn is_refresh_retryable(err: &AuthError) -> bool {
    match err {
        AuthError::Network(_) => true,
        AuthError::Firebase { code, .. } => code
            .parse::<u16>()
            .map(|c| c >= 500 || c == 429)
            .unwrap_or(false),
        AuthError::InvalidCredentials
        | AuthError::TokenExpired
        | AuthError::EmailNotVerified
        | AuthError::AccountDisabled
        | AuthError::MfaRequired { .. }
        | AuthError::NoStoredCredentials
        | AuthError::Keyring(_)
        | AuthError::OAuth { .. }
        | AuthError::UserCancelled
        | AuthError::NetworkUnreachable { .. } => false,
    }
}

pub struct AuthService {
    client: reqwest::Client,
    config: AuthClientConfig,
    tokens: Arc<RwLock<Option<AuthTokens>>>,
}

impl AuthService {
    pub fn new(config: AuthClientConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            config,
            tokens: Arc::new(RwLock::new(None)),
        }
    }

    /// Sign in with email and password
    #[tracing::instrument(skip_all, fields(email = %email))]
    pub async fn sign_in_email(
        &self,
        email: &str,
        password: &str,
    ) -> Result<AuthTokens, AuthError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            email: &'a str,
            password: &'a str,
            return_secure_token: bool,
        }

        let url = format!(
            "{}/accounts:signInWithPassword?key={}",
            FIREBASE_AUTH_URL, self.config.firebase_api_key
        );

        let resp = self
            .client
            .post(&url)
            .json(&Request {
                email,
                password,
                return_secure_token: true,
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            let err: FirebaseErrorResponse = resp.json().await?;
            let auth_err = match err.error.message.as_str() {
                "EMAIL_NOT_FOUND" | "INVALID_PASSWORD" | "INVALID_LOGIN_CREDENTIALS" => {
                    AuthError::InvalidCredentials
                }
                "USER_DISABLED" => AuthError::AccountDisabled,
                msg => AuthError::Firebase {
                    code: err.error.code.to_string(),
                    message: msg.to_string(),
                },
            };
            tracing::warn!(reason = %auth_err, "email sign-in failed");
            return Err(auth_err);
        }

        let sign_in: SignInResponse = resp.json().await?;
        let expires_in: i64 = sign_in.expires_in.parse().unwrap_or(3600);
        let tokens = AuthTokens {
            id_token: sign_in.id_token,
            refresh_token: sign_in.refresh_token,
            expires_at: Utc::now() + Duration::seconds(expires_in),
        };

        self.store_tokens(&tokens).await?;
        tracing::info!("email sign-in ok");
        Ok(tokens)
    }

    /// Sign in with Google via OAuth 2.0 PKCE loopback + Firebase `signInWithIdp`.
    #[tracing::instrument(skip_all)]
    pub async fn sign_in_google(&self) -> Result<AuthTokens, AuthError> {
        self.sign_in_with_idp(OAuthProvider::Google).await
    }

    /// Sign in with Microsoft via OAuth 2.0 PKCE loopback + Firebase `signInWithIdp`.
    #[tracing::instrument(skip_all)]
    pub async fn sign_in_microsoft(&self) -> Result<AuthTokens, AuthError> {
        self.sign_in_with_idp(OAuthProvider::Microsoft).await
    }

    #[tracing::instrument(skip_all, fields(provider = %provider.firebase_provider_id()))]
    async fn sign_in_with_idp(&self, provider: OAuthProvider) -> Result<AuthTokens, AuthError> {
        let (client_id, client_secret) = match provider {
            OAuthProvider::Google => (
                self.config.google_oauth_client_id.clone(),
                Some(self.config.google_oauth_client_secret.clone()),
            ),
            OAuthProvider::Microsoft => (self.config.microsoft_oauth_client_id.clone(), None),
        };
        let provider_id_token = oauth::run_pkce_flow(provider, client_id, client_secret).await?;

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            post_body: String,
            request_uri: &'a str,
            return_secure_token: bool,
            return_idp_credential: bool,
        }

        let post_body = format!(
            "id_token={}&providerId={}",
            provider_id_token,
            provider.firebase_provider_id()
        );
        let url = format!(
            "{}/accounts:signInWithIdp?key={}",
            FIREBASE_AUTH_URL, self.config.firebase_api_key
        );

        let resp = self
            .client
            .post(&url)
            .json(&Request {
                post_body,
                request_uri: "http://localhost",
                return_secure_token: true,
                return_idp_credential: true,
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            let err: FirebaseErrorResponse = resp.json().await?;
            let auth_err = match err.error.message.as_str() {
                "USER_DISABLED" => AuthError::AccountDisabled,
                msg => AuthError::Firebase {
                    code: err.error.code.to_string(),
                    message: msg.to_string(),
                },
            };
            tracing::warn!(reason = %auth_err, "oauth sign-in failed");
            return Err(auth_err);
        }

        let sign_in: SignInResponse = resp.json().await?;
        let expires_in: i64 = sign_in.expires_in.parse().unwrap_or(3600);
        let tokens = AuthTokens {
            id_token: sign_in.id_token,
            refresh_token: sign_in.refresh_token,
            expires_at: Utc::now() + Duration::seconds(expires_in),
        };

        self.store_tokens(&tokens).await?;
        tracing::info!("oauth sign-in ok");
        Ok(tokens)
    }

    /// Refresh the ID token using the stored refresh token
    #[tracing::instrument(skip_all)]
    pub async fn refresh_token(&self) -> Result<AuthTokens, AuthError> {
        let current = self.tokens.read().await;
        let refresh_token = current
            .as_ref()
            .map(|t| t.refresh_token.clone())
            .or_else(|| self.load_refresh_token_from_keyring().ok())
            .ok_or(AuthError::NoStoredCredentials)?;
        drop(current);

        let url = format!(
            "{}?key={}",
            FIREBASE_TOKEN_URL, self.config.firebase_api_key
        );

        // Token refresh sits on the critical path of every authenticated call
        // during an upload, and the Firebase ID token expires hourly — so a
        // single transient network blip reaching Google's securetoken endpoint
        // must not fail a long-running upload. Retry transport errors (and
        // server 5xx/429) with exponential backoff (1s, 2s, 4s, 8s); a real
        // Firebase 4xx (revoked/invalid refresh token) is terminal and returned
        // immediately. Once the budget is exhausted, surface an actionable
        // prompt instead of a raw transport error.
        const MAX_ATTEMPTS: u32 = 5;
        const INITIAL_DELAY_MS: u64 = 1000;
        const MAX_DELAY_MS: u64 = 8000;

        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self.try_refresh_once(&url, &refresh_token).await {
                Ok(tokens) => {
                    self.store_tokens(&tokens).await?;
                    tracing::debug!(attempt, "token refreshed");
                    return Ok(tokens);
                }
                Err(e) if is_refresh_retryable(&e) && attempt < MAX_ATTEMPTS => {
                    let delay = (INITIAL_DELAY_MS << (attempt - 1)).min(MAX_DELAY_MS);
                    tracing::warn!(
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        delay_ms = delay,
                        "token refresh failed, retrying: {e}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                Err(e) if is_refresh_retryable(&e) => {
                    tracing::warn!(attempt, "token refresh exhausted retries: {e}");
                    return Err(AuthError::NetworkUnreachable { attempts: attempt });
                }
                Err(e) => {
                    tracing::warn!(reason = %e, "token refresh failed (non-retryable)");
                    return Err(e);
                }
            }
        }
    }

    /// A single token-refresh attempt: POST the refresh grant and parse the
    /// response. Transport failures surface as `AuthError::Network`; a non-2xx
    /// Firebase response becomes `AuthError::Firebase`. The retry/backoff policy
    /// lives in the caller, [`Self::refresh_token`].
    async fn try_refresh_once(
        &self,
        url: &str,
        refresh_token: &str,
    ) -> Result<AuthTokens, AuthError> {
        let resp = self
            .client
            .post(url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let err: FirebaseErrorResponse = resp.json().await?;
            return Err(AuthError::Firebase {
                code: err.error.code.to_string(),
                message: err.error.message,
            });
        }

        let refresh: RefreshResponse = resp.json().await?;
        let expires_in: i64 = refresh.expires_in.parse().unwrap_or(3600);
        Ok(AuthTokens {
            id_token: refresh.id_token,
            refresh_token: refresh.refresh_token,
            expires_at: Utc::now() + Duration::seconds(expires_in),
        })
    }

    /// Get a valid ID token, refreshing if needed
    pub async fn get_id_token(&self) -> Result<String, AuthError> {
        let tokens = self.tokens.read().await;
        if let Some(ref t) = *tokens {
            // Refresh 5 minutes before expiry
            if t.expires_at > Utc::now() + Duration::minutes(5) {
                return Ok(t.id_token.clone());
            }
        }
        drop(tokens);

        tracing::debug!("id token expiring soon — refreshing");
        let refreshed = self.refresh_token().await?;
        Ok(refreshed.id_token)
    }

    /// Try to restore session from keyring on startup
    pub async fn try_restore_session(&self) -> Result<AuthTokens, AuthError> {
        self.refresh_token().await
    }

    /// Sign out: clear tokens and keyring
    pub async fn sign_out(&self) {
        *self.tokens.write().await = None;
        let _ = self.delete_refresh_token_from_keyring();
    }

    /// Check if user is authenticated
    pub async fn is_authenticated(&self) -> bool {
        self.tokens.read().await.is_some()
    }

    async fn store_tokens(&self, tokens: &AuthTokens) -> Result<(), AuthError> {
        *self.tokens.write().await = Some(tokens.clone());
        self.save_refresh_token_to_keyring(&tokens.refresh_token)?;
        Ok(())
    }

    fn save_refresh_token_to_keyring(&self, token: &str) -> Result<(), AuthError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| AuthError::Keyring(e.to_string()))?;
        entry
            .set_password(token)
            .map_err(|e| AuthError::Keyring(e.to_string()))
    }

    fn load_refresh_token_from_keyring(&self) -> Result<String, AuthError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| AuthError::Keyring(e.to_string()))?;
        entry
            .get_password()
            .map_err(|e| AuthError::Keyring(e.to_string()))
    }

    fn delete_refresh_token_from_keyring(&self) -> Result<(), AuthError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| AuthError::Keyring(e.to_string()))?;
        entry
            .delete_credential()
            .map_err(|e| AuthError::Keyring(e.to_string()))
    }
}

/// Spawn a background task that refreshes the token every 50 minutes
pub fn spawn_token_refresh(auth: Arc<AuthService>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(50 * 60));
        loop {
            interval.tick().await;
            if auth.is_authenticated().await {
                match auth.refresh_token().await {
                    Ok(_) => tracing::debug!("Token refreshed successfully"),
                    Err(e) => tracing::warn!("Token refresh failed: {e}"),
                }
            }
        }
    })
}
