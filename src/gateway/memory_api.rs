//! REST API handlers for direct memory access.
//!
//! Thin HTTP wrappers around the existing `Arc<dyn Memory>` from `AppState`.
//! No new database — reuses the existing memory backend (SQLite, Lucid, etc.).

use super::auth;
use super::responses;
use super::AppState;
use crate::memory::parse_category;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};

/// POST /memory — store a memory
#[derive(serde::Deserialize)]
pub struct StoreBody {
    pub key: String,
    pub content: String,
    pub category: Option<String>,
    pub session_id: Option<String>,
}

pub async fn handle_store(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<StoreBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let client_key = super::client_key_from_headers(&headers);
    if !state.rate_limiter.allow_webhook(&client_key) {
        return responses::err(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Please retry later.",
        );
    }

    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return responses::err(
                StatusCode::BAD_REQUEST,
                &format!("Invalid JSON: {e}"),
            );
        }
    };

    let category = body
        .category
        .as_deref()
        .map(parse_category)
        .unwrap_or(crate::memory::MemoryCategory::Core);

    match state
        .mem
        .store(
            &body.key,
            &body.content,
            category,
            body.session_id.as_deref(),
        )
        .await
    {
        Ok(()) => responses::created(serde_json::json!({
            "key": body.key,
            "stored": true,
        })),
        Err(e) => {
            tracing::error!("Memory store error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to store memory")
        }
    }
}

/// GET /memory — list memories
#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    pub category: Option<String>,
    pub session_id: Option<String>,
}

pub async fn handle_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let client_key = super::client_key_from_headers(&headers);
    if !state.rate_limiter.allow_webhook(&client_key) {
        return responses::err(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Please retry later.",
        );
    }

    let category = query.category.as_deref().map(parse_category);

    match state
        .mem
        .list(category.as_ref(), query.session_id.as_deref())
        .await
    {
        Ok(entries) => responses::ok(entries),
        Err(e) => {
            tracing::error!("Memory list error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list memories")
        }
    }
}

/// GET /memory/search — vector + keyword search
#[derive(serde::Deserialize)]
pub struct SearchQuery {
    pub query: String,
    pub limit: Option<usize>,
    pub session_id: Option<String>,
}

pub async fn handle_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let client_key = super::client_key_from_headers(&headers);
    if !state.rate_limiter.allow_webhook(&client_key) {
        return responses::err(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Please retry later.",
        );
    }

    let limit = query.limit.unwrap_or(5).min(100);

    match state
        .mem
        .recall(&query.query, limit, query.session_id.as_deref())
        .await
    {
        Ok(entries) => responses::ok(entries),
        Err(e) => {
            tracing::error!("Memory search error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to search memories")
        }
    }
}

/// GET /memory/key/{key} — get memory by key
pub async fn handle_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let client_key = super::client_key_from_headers(&headers);
    if !state.rate_limiter.allow_webhook(&client_key) {
        return responses::err(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Please retry later.",
        );
    }

    match state.mem.get(&key).await {
        Ok(Some(entry)) => responses::ok(entry),
        Ok(None) => responses::not_found("Memory"),
        Err(e) => {
            tracing::error!("Memory get error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to get memory")
        }
    }
}

/// DELETE /memory/key/{key} — forget memory by key
pub async fn handle_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let client_key = super::client_key_from_headers(&headers);
    if !state.rate_limiter.allow_webhook(&client_key) {
        return responses::err(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many requests. Please retry later.",
        );
    }

    match state.mem.forget(&key).await {
        Ok(true) => responses::ok(serde_json::json!({
            "key": key,
            "deleted": true,
        })),
        Ok(false) => responses::not_found("Memory"),
        Err(e) => {
            tracing::error!("Memory delete error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete memory")
        }
    }
}

/// GET /memory/count — count total memories
pub async fn handle_count(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    match state.mem.count().await {
        Ok(count) => responses::ok(serde_json::json!({ "count": count })),
        Err(e) => {
            tracing::error!("Memory count error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to count memories")
        }
    }
}
