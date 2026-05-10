//! OAuth 2.0 Authorization Code flow with PKCE for desktop clients.
//!
//! Per RFC 8252, native apps use a loopback redirect (`http://127.0.0.1:<port>`)
//! so the OS browser can hand control back without a custom URI scheme.
//! The client IDs we hold are not secrets — PKCE is what proves possession —
//! so no client_secret is ever sent.
//!
//! Both Google and Microsoft return an OIDC `id_token` in the token-exchange
//! response when `openid` is included in scopes. That `id_token` is what
//! Firebase's `accounts:signInWithIdp` endpoint wants to convert into a
//! Firebase ID token.
//!
//! We use `oauth2` only for PKCE challenge/verifier generation and for
//! building the provider authorize URL. The token-exchange POST is
//! hand-rolled against our workspace `reqwest`, because `oauth2` v5's
//! strongly-typed response layer is tied to its own vendored `reqwest`
//! version and does not compose with the workspace's.

use crate::error::AuthError;
use oauth2::basic::BasicClient;
use oauth2::{AuthUrl, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl, Scope, TokenUrl};
use serde::Deserialize;
use std::net::SocketAddr;
use std::process::Command;
use std::time::Duration;
use url::Url;

#[derive(Clone, Copy, Debug)]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

impl OAuthProvider {
    pub fn firebase_provider_id(self) -> &'static str {
        match self {
            OAuthProvider::Google => "google.com",
            OAuthProvider::Microsoft => "microsoft.com",
        }
    }

    fn auth_url(self) -> &'static str {
        match self {
            OAuthProvider::Google => "https://accounts.google.com/o/oauth2/v2/auth",
            OAuthProvider::Microsoft => {
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
            }
        }
    }

    fn token_url(self) -> &'static str {
        match self {
            OAuthProvider::Google => "https://oauth2.googleapis.com/token",
            OAuthProvider::Microsoft => {
                "https://login.microsoftonline.com/common/oauth2/v2.0/token"
            }
        }
    }

    fn scopes(self) -> &'static [&'static str] {
        // `openid` triggers OIDC id_token issuance on both providers.
        match self {
            OAuthProvider::Google | OAuthProvider::Microsoft => &["openid", "email", "profile"],
        }
    }

    fn display(self) -> &'static str {
        match self {
            OAuthProvider::Google => "google",
            OAuthProvider::Microsoft => "microsoft",
        }
    }
}

/// Body of a successful token-endpoint response from Google or Microsoft.
/// Only `id_token` is load-bearing for Firebase; the access/refresh tokens
/// are ignored because Firebase issues its own.
#[derive(Deserialize)]
struct TokenResponseBody {
    id_token: Option<String>,
}

/// Drive the full PKCE loopback flow end to end and return the provider's
/// OIDC `id_token`, which the caller then exchanges at Firebase's
/// `signInWithIdp` endpoint.
pub(super) async fn run_pkce_flow(
    provider: OAuthProvider,
    client_id: String,
    client_secret: Option<String>,
) -> Result<String, AuthError> {
    // Bind loopback listener first so we can use the actual port in the
    // redirect URI. Port 0 = OS-assigned, which avoids collisions when
    // multiple copies of the app race.
    let listener =
        std::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).map_err(|e| {
            AuthError::OAuth {
                provider: provider.display().to_string(),
                message: format!("Failed to bind loopback listener: {e}"),
            }
        })?;
    let local_addr = listener.local_addr().map_err(|e| AuthError::OAuth {
        provider: provider.display().to_string(),
        message: format!("Failed to read loopback address: {e}"),
    })?;
    let redirect_uri = format!("http://127.0.0.1:{}", local_addr.port());

    let server =
        tiny_http::Server::from_listener(listener, None).map_err(|e| AuthError::OAuth {
            provider: provider.display().to_string(),
            message: format!("Failed to start loopback server: {e}"),
        })?;

    let auth_url = AuthUrl::new(provider.auth_url().to_string()).map_err(|e| AuthError::OAuth {
        provider: provider.display().to_string(),
        message: format!("Invalid auth URL: {e}"),
    })?;
    let token_url =
        TokenUrl::new(provider.token_url().to_string()).map_err(|e| AuthError::OAuth {
            provider: provider.display().to_string(),
            message: format!("Invalid token URL: {e}"),
        })?;
    let redirect_url = RedirectUrl::new(redirect_uri.clone()).map_err(|e| AuthError::OAuth {
        provider: provider.display().to_string(),
        message: format!("Invalid redirect URL: {e}"),
    })?;

    let client = BasicClient::new(ClientId::new(client_id.clone()))
        .set_auth_uri(auth_url)
        .set_token_uri(token_url)
        .set_redirect_uri(redirect_url);

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let mut authorize_req = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(pkce_challenge);
    for scope in provider.scopes() {
        authorize_req = authorize_req.add_scope(Scope::new((*scope).to_string()));
    }
    let (authorize_url, csrf_state) = authorize_req.url();

    open_url_in_browser(authorize_url.as_str()).map_err(|message| AuthError::OAuth {
        provider: provider.display().to_string(),
        message,
    })?;

    // Block the async task on the synchronous loopback listener. Doing this
    // inside `spawn_blocking` keeps the runtime responsive.
    let provider_display = provider.display().to_string();
    let expected_state = csrf_state.secret().clone();
    let loopback_result = tokio::task::spawn_blocking(move || {
        wait_for_code(server, &expected_state, &provider_display)
    })
    .await
    .map_err(|e| AuthError::OAuth {
        provider: provider.display().to_string(),
        message: format!("Loopback task panicked: {e}"),
    })??;

    exchange_code_for_id_token(
        provider,
        &client_id,
        client_secret.as_deref(),
        &loopback_result.code,
        pkce_verifier.secret(),
        &redirect_uri,
    )
    .await
}

async fn exchange_code_for_id_token(
    provider: OAuthProvider,
    client_id: &str,
    client_secret: Option<&str>,
    code: &str,
    pkce_verifier: &str,
    redirect_uri: &str,
) -> Result<String, AuthError> {
    let http = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AuthError::OAuth {
            provider: provider.display().to_string(),
            message: format!("Failed to build HTTP client: {e}"),
        })?;

    let mut form: Vec<(&str, &str)> = vec![
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", pkce_verifier),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }

    let resp = http.post(provider.token_url()).form(&form).send().await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AuthError::OAuth {
            provider: provider.display().to_string(),
            message: format!("Token exchange failed (HTTP {status}): {body}"),
        });
    }

    let body: TokenResponseBody = resp.json().await.map_err(|e| AuthError::OAuth {
        provider: provider.display().to_string(),
        message: format!("Failed to parse token response: {e}"),
    })?;

    body.id_token.ok_or_else(|| AuthError::OAuth {
        provider: provider.display().to_string(),
        message: "Provider did not return an id_token — is `openid` scope granted?".into(),
    })
}

struct LoopbackCode {
    code: String,
}

/// Wait for the single browser redirect the provider will make to our
/// loopback, then respond with a small "you may close this window" page so
/// the user isn't staring at a blank tab.
///
/// The caller is expected to have started `server` bound to 127.0.0.1 and
/// handed the matching redirect URI to the provider.
fn wait_for_code(
    server: tiny_http::Server,
    expected_state: &str,
    provider_display: &str,
) -> Result<LoopbackCode, AuthError> {
    // A single inbound GET is all we need. The browser may also request
    // /favicon.ico; ignore anything that isn't the auth callback.
    loop {
        let request = server.recv().map_err(|e| AuthError::OAuth {
            provider: provider_display.to_string(),
            message: format!("Loopback recv failed: {e}"),
        })?;
        let url = request.url().to_string();
        let Some(query) = url.strip_prefix("/?") else {
            let _ = request.respond(tiny_http::Response::from_string("").with_status_code(404));
            continue;
        };
        if query.is_empty() {
            let _ = request.respond(tiny_http::Response::from_string("").with_status_code(404));
            continue;
        }

        // Parse `?code=...&state=...` or `?error=access_denied&...`.
        let full = format!("http://127.0.0.1/?{query}");
        let parsed = Url::parse(&full).map_err(|e| AuthError::OAuth {
            provider: provider_display.to_string(),
            message: format!("Failed to parse callback URL: {e}"),
        })?;
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();

        if let Some(err) = params.get("error") {
            let html = closed_page(
                "Sign-in failed",
                "You can close this window and return to Linewise.",
            );
            respond_html(request, &html);
            if err == "access_denied" {
                return Err(AuthError::UserCancelled);
            }
            return Err(AuthError::OAuth {
                provider: provider_display.to_string(),
                message: params
                    .get("error_description")
                    .cloned()
                    .unwrap_or_else(|| err.clone()),
            });
        }

        let state = params.get("state").cloned().unwrap_or_default();
        if state != expected_state {
            let html = closed_page(
                "Sign-in failed",
                "State mismatch. For your safety the sign-in was cancelled.",
            );
            respond_html(request, &html);
            return Err(AuthError::OAuth {
                provider: provider_display.to_string(),
                message: "State parameter did not match expected value".into(),
            });
        }

        let Some(code) = params.get("code").cloned() else {
            let _ = request.respond(tiny_http::Response::from_string("").with_status_code(400));
            return Err(AuthError::OAuth {
                provider: provider_display.to_string(),
                message: "Callback missing `code` parameter".into(),
            });
        };

        let html = closed_page(
            "Sign-in complete",
            "You can close this window and return to Linewise.",
        );
        respond_html(request, &html);
        return Ok(LoopbackCode { code });
    }
}

fn is_wsl2() -> bool {
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/version")
            .map(|v| v.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
    }
}

fn open_url_in_browser(url: &str) -> Result<(), String> {
    let browser_result = webbrowser::open(url);
    if browser_result.is_ok() {
        return Ok(());
    }
    let browser_err = browser_result.expect_err("checked is_ok above");

    if !is_wsl2() {
        return Err(format!("Failed to open browser: {browser_err}"));
    }

    tracing::debug!("webbrowser crate failed on WSL2, trying cmd.exe fallback");

    let escaped = url.replace('^', "^^").replace('&', "^&");
    Command::new("cmd.exe")
        .args(["/C", "start", "", &escaped])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("WSL2 cmd.exe fallback failed: {e}"))?;

    Ok(())
}

fn respond_html(request: tiny_http::Request, html: &str) {
    let header =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("static header bytes are valid");
    let _ = request.respond(tiny_http::Response::from_string(html).with_header(header));
}

fn closed_page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{font-family:-apple-system,system-ui,sans-serif;display:flex;\
         align-items:center;justify-content:center;height:100vh;margin:0;color:#111}}\
         .card{{max-width:360px;padding:24px;text-align:center}}\
         h1{{font-size:18px;margin:0 0 8px}}p{{color:#555;font-size:14px;margin:0}}</style>\
         </head><body><div class=\"card\"><h1>{title}</h1><p>{body}</p></div></body></html>"
    )
}
