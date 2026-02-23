//! Consistent JSON response envelopes for gateway API endpoints.

use axum::http::StatusCode;
use axum::response::Json;
use serde::Serialize;

/// Success response: `{"success": true, "data": ...}`
pub fn ok<T: Serialize>(data: T) -> (StatusCode, Json<serde_json::Value>) {
    let body = serde_json::json!({
        "success": true,
        "data": serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
    });
    (StatusCode::OK, Json(body))
}

/// Created response (201): `{"success": true, "data": ...}`
pub fn created<T: Serialize>(data: T) -> (StatusCode, Json<serde_json::Value>) {
    let body = serde_json::json!({
        "success": true,
        "data": serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
    });
    (StatusCode::CREATED, Json(body))
}

/// Error response: `{"success": false, "error": "..."}`
pub fn err(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    let body = serde_json::json!({
        "success": false,
        "error": msg,
    });
    (status, Json(body))
}

/// Not-found shorthand (404): `{"success": false, "error": "<what> not found"}`
pub fn not_found(what: &str) -> (StatusCode, Json<serde_json::Value>) {
    err(StatusCode::NOT_FOUND, &format!("{what} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_wraps_data() {
        let (status, Json(body)) = ok(serde_json::json!({"key": "value"}));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
        assert_eq!(body["data"]["key"], "value");
    }

    #[test]
    fn created_returns_201() {
        let (status, Json(body)) = created("hello");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["success"], true);
        assert_eq!(body["data"], "hello");
    }

    #[test]
    fn err_wraps_message() {
        let (status, Json(body)) = err(StatusCode::BAD_REQUEST, "bad input");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["success"], false);
        assert_eq!(body["error"], "bad input");
    }

    #[test]
    fn not_found_formats_message() {
        let (status, Json(body)) = not_found("Memory");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "Memory not found");
    }
}
