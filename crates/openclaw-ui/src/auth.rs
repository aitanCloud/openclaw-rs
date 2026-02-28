use std::sync::Arc;

use argon2::{Argon2, PasswordVerifier};
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use sha2::{Digest, Sha256};

use crate::state::AppState;

/// Token fingerprint: first 24 hex chars of SHA-256(raw_token).
/// Safe for logging -- never log the raw token.
pub fn token_fingerprint(raw_token: &str) -> String {
    let hash = Sha256::digest(raw_token.as_bytes());
    let full_hex = hex::encode(hash);
    full_hex[..24].to_string()
}

/// Verify a raw token against an argon2id hash using constant-time comparison.
pub fn verify_token(raw_token: &str, hash: &str) -> bool {
    use argon2::PasswordHash;
    let parsed = match PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(raw_token.as_bytes(), &parsed)
        .is_ok()
}

/// Extract the bearer token from an Authorization header value.
pub fn extract_bearer(header_value: &str) -> Option<&str> {
    let trimmed = header_value.trim();
    if trimmed.len() > 7
        && trimmed[..7].eq_ignore_ascii_case("bearer ")
    {
        let token = trimmed[7..].trim();
        if token.is_empty() {
            None
        } else {
            Some(token)
        }
    } else {
        None
    }
}

/// JSON error body for 401 responses.
fn unauthorized_response() -> Response {
    let body = serde_json::json!({
        "error": {
            "code": "UNAUTHORIZED",
            "message": "Missing or invalid bearer token"
        }
    });
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

/// Axum middleware layer that enforces bearer token authentication.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let raw_token = match auth_header.and_then(extract_bearer) {
        Some(t) => t,
        None => {
            tracing::warn!("auth: missing or malformed Authorization header");
            return unauthorized_response();
        }
    };

    let fingerprint = token_fingerprint(raw_token);

    if !verify_token(raw_token, &state.config.auth_token_hash) {
        tracing::warn!(
            token_fingerprint = %fingerprint,
            "auth: invalid token"
        );
        return unauthorized_response();
    }

    tracing::debug!(token_fingerprint = %fingerprint, "auth: token verified");
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── token_fingerprint ──

    #[test]
    fn fingerprint_is_24_hex_chars() {
        let fp = token_fingerprint("my-secret-token");
        assert_eq!(fp.len(), 24);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fingerprint_deterministic() {
        let fp1 = token_fingerprint("test-token");
        let fp2 = token_fingerprint("test-token");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_differs_for_different_tokens() {
        let fp1 = token_fingerprint("token-a");
        let fp2 = token_fingerprint("token-b");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn fingerprint_empty_token() {
        let fp = token_fingerprint("");
        assert_eq!(fp.len(), 24);
    }

    // ── extract_bearer ──

    #[test]
    fn extract_bearer_valid() {
        assert_eq!(extract_bearer("Bearer my-token"), Some("my-token"));
    }

    #[test]
    fn extract_bearer_case_insensitive() {
        assert_eq!(extract_bearer("bearer my-token"), Some("my-token"));
        assert_eq!(extract_bearer("BEARER my-token"), Some("my-token"));
        assert_eq!(extract_bearer("BeArEr my-token"), Some("my-token"));
    }

    #[test]
    fn extract_bearer_with_leading_trailing_whitespace() {
        assert_eq!(extract_bearer("  Bearer my-token  "), Some("my-token"));
    }

    #[test]
    fn extract_bearer_empty_token() {
        assert_eq!(extract_bearer("Bearer "), None);
        assert_eq!(extract_bearer("Bearer   "), None);
    }

    #[test]
    fn extract_bearer_missing_scheme() {
        assert_eq!(extract_bearer("my-token"), None);
    }

    #[test]
    fn extract_bearer_wrong_scheme() {
        assert_eq!(extract_bearer("Basic dXNlcjpwYXNz"), None);
    }

    #[test]
    fn extract_bearer_empty_string() {
        assert_eq!(extract_bearer(""), None);
    }

    // ── verify_token (argon2id) ──

    #[test]
    fn verify_token_valid_hash() {
        // Pre-computed argon2id hash for "test-secret"
        use argon2::password_hash::{PasswordHasher, SaltString};
        use argon2::Argon2;
        let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
        let hash = Argon2::default()
            .hash_password(b"test-secret", &salt)
            .unwrap()
            .to_string();

        assert!(verify_token("test-secret", &hash));
    }

    #[test]
    fn verify_token_wrong_password() {
        use argon2::password_hash::{PasswordHasher, SaltString};
        use argon2::Argon2;
        let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
        let hash = Argon2::default()
            .hash_password(b"correct-password", &salt)
            .unwrap()
            .to_string();

        assert!(!verify_token("wrong-password", &hash));
    }

    #[test]
    fn verify_token_invalid_hash_format() {
        assert!(!verify_token("anything", "not-a-valid-hash"));
    }

    #[test]
    fn verify_token_empty_hash() {
        assert!(!verify_token("token", ""));
    }
}
