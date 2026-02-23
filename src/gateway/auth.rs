//! Reusable Bearer-token authentication for gateway handlers.

use crate::security::pairing::PairingGuard;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Json;

/// Verify the Bearer token from `Authorization` header against the `PairingGuard`.
///
/// Returns `Ok(())` if auth is not required or the token is valid.
/// Returns `Err((StatusCode, Json))` with a 401 response if the token is missing/invalid.
pub fn require_auth(
    pairing: &PairingGuard,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !pairing.require_pairing() {
        return Ok(());
    }

    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("");

    if pairing.is_authenticated(token) {
        Ok(())
    } else {
        let err = serde_json::json!({
            "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
        });
        Err((StatusCode::UNAUTHORIZED, Json(err)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn guard_no_pairing() -> PairingGuard {
        PairingGuard::new(false, &[])
    }

    fn guard_with_token(token: &str) -> PairingGuard {
        PairingGuard::new(true, &[token.to_string()])
    }

    #[test]
    fn no_pairing_required_always_passes() {
        let guard = guard_no_pairing();
        assert!(require_auth(&guard, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn missing_header_is_rejected() {
        let guard = guard_with_token("secret123");
        let result = require_auth(&guard, &HeaderMap::new());
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn wrong_token_is_rejected() {
        let guard = guard_with_token("correct_token");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong_token"),
        );
        let result = require_auth(&guard, &headers);
        assert!(result.is_err());
    }

    #[test]
    fn correct_token_passes() {
        let guard = guard_with_token("my_secret");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str("Bearer my_secret").unwrap(),
        );
        let result = require_auth(&guard, &headers);
        assert!(result.is_ok());
    }
}
