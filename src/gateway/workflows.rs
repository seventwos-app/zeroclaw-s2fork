//! Workflow discovery and execution handlers.
//!
//! GET  /workflows                    — list all workflow templates grouped by category
//! POST /workflows/:category/:id/run  — fire-and-forget agent run for a workflow

use crate::agent::loop_::{agent_turn, ToolCallRecord};
use crate::gateway::AppState;
use crate::providers::ChatMessage;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use std::fs;

type HandlerResult = (StatusCode, Json<serde_json::Value>);

fn ok(body: serde_json::Value) -> HandlerResult {
    (StatusCode::OK, Json(body))
}

fn err(status: StatusCode, msg: &str) -> HandlerResult {
    (status, Json(serde_json::json!({ "error": msg })))
}

/// Read `ZEROCLAW_WORKFLOWS_DIR` env var, fall back to sibling `workflows/` directory.
fn workflows_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ZEROCLAW_WORKFLOWS_DIR") {
        return std::path::PathBuf::from(dir);
    }
    // Dev fallback via CARGO_MANIFEST_DIR baked in at compile time
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/workflows"))
}

/// Parse the first heading and first description line from a README.md.
fn parse_readme(path: &std::path::Path) -> (String, String) {
    let Ok(contents) = fs::read_to_string(path) else {
        return (String::new(), String::new());
    };
    let mut name = String::new();
    let mut desc = String::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if name.is_empty() {
            if let Some(rest) = trimmed.strip_prefix("# ") {
                name = rest.trim().to_string();
                continue;
            }
        }
        if desc.is_empty() && !trimmed.is_empty() && !trimmed.starts_with('#') {
            desc = trimmed.to_string();
        }
        if !name.is_empty() && !desc.is_empty() {
            break;
        }
    }
    (name, desc)
}

/// GET /workflows — walk the workflows directory and return grouped JSON.
pub async fn handle_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> HandlerResult {
    if let Err(resp) = crate::gateway::auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let base = workflows_dir();
    if !base.is_dir() {
        return ok(serde_json::json!({ "categories": [] }));
    }

    let Ok(entries) = fs::read_dir(&base) else {
        return ok(serde_json::json!({ "categories": [] }));
    };

    let mut cat_entries: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && !e.file_name().to_string_lossy().starts_with('_')
        })
        .collect();
    cat_entries.sort_by_key(|e| e.file_name());

    let mut categories: Vec<serde_json::Value> = Vec::new();

    for cat_entry in cat_entries {
        let category = cat_entry.file_name().to_string_lossy().to_string();
        let Ok(wf_entries) = fs::read_dir(cat_entry.path()) else {
            continue;
        };

        let mut wf_list: Vec<_> = wf_entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();
        wf_list.sort_by_key(|e| e.file_name());

        let mut workflows: Vec<serde_json::Value> = Vec::new();
        for wf_entry in wf_list {
            let id = wf_entry.file_name().to_string_lossy().to_string();
            let readme = wf_entry.path().join("README.md");
            let (name, description) = if readme.exists() {
                parse_readme(&readme)
            } else {
                (id.clone(), String::new())
            };
            let display_name = if name.is_empty() { id.clone() } else { name };
            let has_prompt = wf_entry.path().join("prompts").is_dir();
            workflows.push(serde_json::json!({
                "id": id,
                "name": display_name,
                "description": description,
                "has_prompt": has_prompt,
            }));
        }

        if !workflows.is_empty() {
            categories.push(serde_json::json!({
                "category": category,
                "workflows": workflows,
            }));
        }
    }

    ok(serde_json::json!({ "categories": categories }))
}

/// POST /workflows/:category/:id/run — fire-and-forget agent turn.
pub async fn handle_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((category, id)): Path<(String, String)>,
) -> HandlerResult {
    if let Err(resp) = crate::gateway::auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    // Reject path traversal
    if category.contains("..") || category.contains('/') || id.contains("..") || id.contains('/') {
        return err(StatusCode::BAD_REQUEST, "Invalid path component");
    }

    let base = workflows_dir();
    let prompts_dir = base.join(&category).join(&id).join("prompts");
    if !prompts_dir.is_dir() {
        return err(StatusCode::NOT_FOUND, "Workflow or prompts not found");
    }

    let prompt_text = match find_first_prompt(&prompts_dir) {
        Some(text) => text,
        None => return err(StatusCode::NOT_FOUND, "No prompt file found"),
    };

    // Clone state fields needed for the spawned task
    let provider = state.provider.clone();
    let tools_registry = state.tools_registry.clone();
    let observer = state.observer.clone();
    let provider_name = state.provider_name.clone();
    let model = state.model.clone();
    let temperature = state.temperature;
    let system_prompt = state.system_prompt.clone();
    let id_for_response = id.clone();

    tokio::spawn(async move {
        let mut history: Vec<ChatMessage> = vec![
            ChatMessage::system(system_prompt.as_ref()),
            ChatMessage {
                role: "user".into(),
                content: prompt_text,
            },
        ];
        let mut records: Vec<ToolCallRecord> = Vec::new();
        match agent_turn(
            provider.as_ref(),
            &mut history,
            &tools_registry,
            observer.as_ref(),
            &provider_name,
            &model,
            temperature,
            true,
            Some(&mut records),
        )
        .await
        {
            Ok(response) => {
                tracing::info!(workflow = %id, response_len = response.len(), "Workflow run complete");
            }
            Err(e) => {
                tracing::warn!(workflow = %id, error = %e, "Workflow run failed");
            }
        }
    });

    (StatusCode::ACCEPTED, Json(serde_json::json!({ "status": "accepted", "workflow": id_for_response })))
}

fn find_first_prompt(dir: &std::path::Path) -> Option<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return None;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "md")
                .unwrap_or(false)
        })
        .collect();
    files.sort_by_key(|e| e.file_name());
    files
        .first()
        .and_then(|e| fs::read_to_string(e.path()).ok())
}
