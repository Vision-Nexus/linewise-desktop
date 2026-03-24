use crate::error::AuthError;
use crate::models::AuthTokens;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

const FIREBASE_AUTH_URL: &str = "https://identitytoolkit.googleapis.com/v1";
const FIREBASE_TOKEN_URL: &str = "https://securetoken.googleapis.com/v1/token";
const KEYRING_SERVICE: &str = "linewise-desktop";
const KEYRING_USER: &str = "refresh_token";

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

pub struct AuthService {
    client: reqwest::Client,
    api_key: String,
    tokens: Arc<RwLock<Option<AuthTokens>>>,
}

impl AuthService {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            tokens: Arc::new(RwLock::new(None)),
        }
    }

    /// Sign in with email and password
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
            FIREBASE_AUTH_URL, self.api_key
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
            return Err(match err.error.message.as_str() {
                "EMAIL_NOT_FOUND" | "INVALID_PASSWORD" | "INVALID_LOGIN_CREDENTIALS" => {
                    AuthError::InvalidCredentials
                }
                "USER_DISABLED" => AuthError::AccountDisabled,
                msg => AuthError::Firebase {
                    code: err.error.code.to_string(),
                    message: msg.to_string(),
                },
            });
        }

        let sign_in: SignInResponse = resp.json().await?;
        let expires_in: i64 = sign_in.expires_in.parse().unwrap_or(3600);
        let tokens = AuthTokens {
            id_token: sign_in.id_token,
            refresh_token: sign_in.refresh_token,
            expires_at: Utc::now() + Duration::seconds(expires_in),
        };

        self.store_tokens(&tokens).await?;
        Ok(tokens)
    }

    /// Refresh the ID token using the stored refresh token
    pub async fn refresh_token(&self) -> Result<AuthTokens, AuthError> {
        let current = self.tokens.read().await;
        let refresh_token = current
            .as_ref()
            .map(|t| t.refresh_token.clone())
            .or_else(|| self.load_refresh_token_from_keyring().ok())
            .ok_or(AuthError::NoStoredCredentials)?;
        drop(current);

        let url = format!("{}?key={}", FIREBASE_TOKEN_URL, self.api_key);

        let resp = self
            .client
            .post(&url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_token),
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
        let tokens = AuthTokens {
            id_token: refresh.id_token,
            refresh_token: refresh.refresh_token,
            expires_at: Utc::now() + Duration::seconds(expires_in),
        };

        self.store_tokens(&tokens).await?;
        Ok(tokens)
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
