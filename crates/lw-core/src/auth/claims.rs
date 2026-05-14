//! Unverified Firebase JWT claim extraction.
//!
//! The Firebase ID token is RS256-signed by Firebase. The backend
//! verifies signature, expiry, audience, and issuer on every API call
//! — that is the security boundary. The desktop never makes a
//! security-critical decision based on local claim inspection.
//!
//! What we do here is decode the JWT payload (the middle base64url
//! segment) so the UI can gate admin-only affordances, e.g. the
//! environment switcher. If the token is tampered with, the user
//! already controls the local session; the worst case is they show
//! themselves an admin UI that does nothing because the backend
//! rejects every request.
//!
//! No signature verification. Do not use these claims to authorize
//! anything beyond local UI rendering.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

/// Subset of the Firebase JWT payload we read on the desktop.
///
/// The full token has many more fields (iss, aud, exp, iat, sub,
/// firebase.identities, ...). We only deserialize what the UI uses,
/// because the goal is "show admin pane or hide it"; everything else
/// is the backend's job to validate and act on.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FirebaseClaims {
    /// `systemRoles` custom claim, e.g. `["admin"]` or `["viewer"]`.
    /// Empty for ordinary tenant users.
    #[serde(rename = "systemRoles")]
    pub system_roles: Vec<String>,
}

impl FirebaseClaims {
    /// True when the user holds at least one system-level role of any
    /// kind (`admin`, `viewer`, `probe`, ...). The environment switcher
    /// uses this gate intentionally — we want viewer-only system users
    /// (who can't edit prod data) to still be able to redirect the
    /// desktop at testing or dev to investigate issues.
    pub fn is_system_user(&self) -> bool {
        !self.system_roles.is_empty()
    }
}

/// Decode the payload of a Firebase JWT into `FirebaseClaims` without
/// verifying the signature. Returns `Default` (i.e. no system roles)
/// for any malformed input — a malformed local token shouldn't crash
/// the app, the backend rejects it on the next request anyway.
pub fn decode_unverified(jwt: &str) -> FirebaseClaims {
    decode_unverified_strict(jwt).unwrap_or_default()
}

fn decode_unverified_strict(jwt: &str) -> Option<FirebaseClaims> {
    let mut parts = jwt.split('.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    // JWS has three dot-separated segments — assert the signature
    // segment exists, even though we don't verify it.
    parts.next()?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    serde_json::from_slice(&payload_bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake JWT (header.payload.signature) where only the
    /// payload's JSON content matters — we never verify the rest.
    fn fake_jwt(payload: &str) -> String {
        let header_b64 = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload);
        format!("{header_b64}.{payload_b64}.signature-irrelevant")
    }

    #[test]
    fn decodes_system_roles() {
        let token = fake_jwt(r#"{"systemRoles":["admin","viewer"],"sub":"abc"}"#);
        let claims = decode_unverified(&token);
        assert_eq!(claims.system_roles, vec!["admin", "viewer"]);
        assert!(claims.is_system_user());
    }

    #[test]
    fn empty_when_field_absent() {
        let token = fake_jwt(r#"{"sub":"abc","iat":1234}"#);
        let claims = decode_unverified(&token);
        assert!(claims.system_roles.is_empty());
        assert!(!claims.is_system_user());
    }

    #[test]
    fn empty_when_field_present_but_empty() {
        let token = fake_jwt(r#"{"systemRoles":[]}"#);
        let claims = decode_unverified(&token);
        assert!(claims.system_roles.is_empty());
        assert!(!claims.is_system_user());
    }

    #[test]
    fn malformed_returns_default() {
        // Two-segment "JWT" — not a real token shape.
        let claims = decode_unverified("not.a-real-token");
        assert!(claims.system_roles.is_empty());
    }

    #[test]
    fn malformed_payload_returns_default() {
        let token = fake_jwt("this-is-not-json");
        let claims = decode_unverified(&token);
        assert!(claims.system_roles.is_empty());
    }
}
