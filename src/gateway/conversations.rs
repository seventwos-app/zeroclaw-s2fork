//! Persistent conversation store for multi-turn threading.
//!
//! Follows the `ResponseCache` pattern — separate SQLite database (`conversations.db`)
//! alongside `brain.db`. Each conversation holds an ordered list of messages that
//! the webhook handler can reload to provide multi-turn context.

use super::auth;
use super::responses;
use super::AppState;
use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Local;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::{Path as FsPath, PathBuf};
use uuid::Uuid;

// ──────────────────────────────────────────────────────────────────────────────
// Data types
// ──────────────────────────────────────────────────────────────────────────────

/// Summary info for a conversation (returned by list/get endpoints).
#[derive(Debug, Clone, Serialize)]
pub struct ConversationInfo {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
}

/// A single message within a conversation.
#[derive(Debug, Clone, Serialize)]
pub struct ConversationMessage {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub tool_calls_json: Option<String>,
    pub created_at: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// ConversationStore
// ──────────────────────────────────────────────────────────────────────────────

pub struct ConversationStore {
    conn: Mutex<Connection>,
    #[allow(dead_code)]
    db_path: PathBuf,
    max_messages: usize,
}

impl ConversationStore {
    /// Open (or create) the conversations database.
    pub fn new(workspace_dir: &FsPath, max_messages: usize) -> Result<Self> {
        let db_dir = workspace_dir.join("memory");
        std::fs::create_dir_all(&db_dir)?;
        let db_path = db_dir.join("conversations.db");

        let conn = Connection::open(&db_path)?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA temp_store   = MEMORY;
             PRAGMA foreign_keys = ON;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS conversations (
                id            TEXT PRIMARY KEY,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                message_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS conversation_messages (
                id              TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                role            TEXT NOT NULL,
                content         TEXT NOT NULL,
                tool_calls_json TEXT,
                created_at      TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cm_conv_id ON conversation_messages(conversation_id);
            CREATE INDEX IF NOT EXISTS idx_cm_created ON conversation_messages(created_at);",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
            max_messages,
        })
    }

    /// Max messages to load for context.
    pub fn max_messages(&self) -> usize {
        self.max_messages
    }

    /// Get or create a conversation by ID.
    pub fn get_or_create(&self, id: &str) -> Result<ConversationInfo> {
        let conn = self.conn.lock();
        let now = Local::now().to_rfc3339();

        // Try to fetch existing
        let existing: Option<ConversationInfo> = conn
            .query_row(
                "SELECT id, created_at, updated_at, message_count FROM conversations WHERE id = ?1",
                params![id],
                |row| {
                    Ok(ConversationInfo {
                        id: row.get(0)?,
                        created_at: row.get(1)?,
                        updated_at: row.get(2)?,
                        message_count: row.get(3)?,
                    })
                },
            )
            .ok();

        if let Some(info) = existing {
            return Ok(info);
        }

        // Create new
        conn.execute(
            "INSERT INTO conversations (id, created_at, updated_at, message_count) VALUES (?1, ?2, ?3, 0)",
            params![id, now, now],
        )?;

        Ok(ConversationInfo {
            id: id.to_string(),
            created_at: now.clone(),
            updated_at: now,
            message_count: 0,
        })
    }

    /// Load the most recent N messages from a conversation, ordered oldest-first.
    pub fn get_recent_messages(&self, conv_id: &str, limit: usize) -> Result<Vec<ConversationMessage>> {
        let conn = self.conn.lock();

        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;

        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, role, content, tool_calls_json, created_at
             FROM conversation_messages
             WHERE conversation_id = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;

        let rows: Vec<ConversationMessage> = stmt
            .query_map(params![conv_id, limit_i64], |row| {
                Ok(ConversationMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_calls_json: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Reverse so oldest messages come first
        let mut messages = rows;
        messages.reverse();
        Ok(messages)
    }

    /// Append a message to a conversation.
    pub fn append_message(
        &self,
        conv_id: &str,
        role: &str,
        content: &str,
        tool_calls_json: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = Local::now().to_rfc3339();
        let msg_id = Uuid::new_v4().to_string();

        conn.execute(
            "INSERT INTO conversation_messages (id, conversation_id, role, content, tool_calls_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![msg_id, conv_id, role, content, tool_calls_json, now],
        )?;

        conn.execute(
            "UPDATE conversations SET message_count = message_count + 1, updated_at = ?1 WHERE id = ?2",
            params![now, conv_id],
        )?;

        Ok(())
    }

    /// List conversations with pagination.
    pub fn list(&self, limit: usize, offset: usize) -> Result<Vec<ConversationInfo>> {
        let conn = self.conn.lock();

        #[allow(clippy::cast_possible_wrap)]
        let (limit_i64, offset_i64) = (limit as i64, offset as i64);

        let mut stmt = conn.prepare(
            "SELECT id, created_at, updated_at, message_count
             FROM conversations
             ORDER BY updated_at DESC
             LIMIT ?1 OFFSET ?2",
        )?;

        let rows = stmt
            .query_map(params![limit_i64, offset_i64], |row| {
                Ok(ConversationInfo {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    updated_at: row.get(2)?,
                    message_count: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
    }

    /// Get a single conversation with its full message history.
    pub fn get(&self, id: &str) -> Result<Option<(ConversationInfo, Vec<ConversationMessage>)>> {
        let conn = self.conn.lock();

        let info: Option<ConversationInfo> = conn
            .query_row(
                "SELECT id, created_at, updated_at, message_count FROM conversations WHERE id = ?1",
                params![id],
                |row| {
                    Ok(ConversationInfo {
                        id: row.get(0)?,
                        created_at: row.get(1)?,
                        updated_at: row.get(2)?,
                        message_count: row.get(3)?,
                    })
                },
            )
            .ok();

        let Some(info) = info else {
            return Ok(None);
        };

        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, role, content, tool_calls_json, created_at
             FROM conversation_messages
             WHERE conversation_id = ?1
             ORDER BY created_at ASC",
        )?;

        let messages = stmt
            .query_map(params![id], |row| {
                Ok(ConversationMessage {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    role: row.get(2)?,
                    content: row.get(3)?,
                    tool_calls_json: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some((info, messages)))
    }

    /// Delete a conversation and all its messages (cascade).
    pub fn delete(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let affected = conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
        Ok(affected > 0)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// HTTP Handlers
// ──────────────────────────────────────────────────────────────────────────────

/// GET /conversations query params
#[derive(serde::Deserialize, Default)]
pub struct ListQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// GET /conversations — list conversations
pub async fn handle_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let Some(ref store) = state.conversation_store else {
        return responses::err(StatusCode::SERVICE_UNAVAILABLE, "Conversation store not available");
    };

    let limit = query.limit.unwrap_or(50).min(200);
    let offset = query.offset.unwrap_or(0);

    match store.list(limit, offset) {
        Ok(conversations) => responses::ok(conversations),
        Err(e) => {
            tracing::error!("Conversation list error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to list conversations")
        }
    }
}

/// GET /conversations/{id} — get conversation with full history
pub async fn handle_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let Some(ref store) = state.conversation_store else {
        return responses::err(StatusCode::SERVICE_UNAVAILABLE, "Conversation store not available");
    };

    match store.get(&id) {
        Ok(Some((info, messages))) => responses::ok(serde_json::json!({
            "conversation": info,
            "messages": messages,
        })),
        Ok(None) => responses::not_found("Conversation"),
        Err(e) => {
            tracing::error!("Conversation get error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to get conversation")
        }
    }
}

/// DELETE /conversations/{id} — delete conversation and all messages
pub async fn handle_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = auth::require_auth(&state.pairing, &headers) {
        return resp;
    }

    let Some(ref store) = state.conversation_store else {
        return responses::err(StatusCode::SERVICE_UNAVAILABLE, "Conversation store not available");
    };

    match store.delete(&id) {
        Ok(true) => responses::ok(serde_json::json!({
            "id": id,
            "deleted": true,
        })),
        Ok(false) => responses::not_found("Conversation"),
        Err(e) => {
            tracing::error!("Conversation delete error: {e}");
            responses::err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete conversation")
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, ConversationStore) {
        let tmp = TempDir::new().unwrap();
        let store = ConversationStore::new(tmp.path(), 50).unwrap();
        (tmp, store)
    }

    #[test]
    fn create_and_get_conversation() {
        let (_tmp, store) = temp_store();
        let info = store.get_or_create("conv-1").unwrap();
        assert_eq!(info.id, "conv-1");
        assert_eq!(info.message_count, 0);

        // Second call returns same conversation
        let info2 = store.get_or_create("conv-1").unwrap();
        assert_eq!(info2.id, "conv-1");
        assert_eq!(info2.created_at, info.created_at);
    }

    #[test]
    fn append_and_retrieve_messages() {
        let (_tmp, store) = temp_store();
        store.get_or_create("conv-2").unwrap();

        store
            .append_message("conv-2", "user", "Hello", None)
            .unwrap();
        store
            .append_message("conv-2", "assistant", "Hi there!", None)
            .unwrap();
        store
            .append_message("conv-2", "user", "How are you?", None)
            .unwrap();

        let messages = store.get_recent_messages("conv-2", 50).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
        assert_eq!(messages[2].content, "How are you?");
    }

    #[test]
    fn recent_messages_respects_limit() {
        let (_tmp, store) = temp_store();
        store.get_or_create("conv-3").unwrap();

        for i in 0..10 {
            store
                .append_message("conv-3", "user", &format!("msg {i}"), None)
                .unwrap();
        }

        let messages = store.get_recent_messages("conv-3", 3).unwrap();
        assert_eq!(messages.len(), 3);
        // Should be the 3 most recent, ordered oldest-first
        assert_eq!(messages[0].content, "msg 7");
        assert_eq!(messages[1].content, "msg 8");
        assert_eq!(messages[2].content, "msg 9");
    }

    #[test]
    fn message_count_increments() {
        let (_tmp, store) = temp_store();
        store.get_or_create("conv-4").unwrap();

        store
            .append_message("conv-4", "user", "one", None)
            .unwrap();
        store
            .append_message("conv-4", "assistant", "two", None)
            .unwrap();

        let (info, _) = store.get("conv-4").unwrap().unwrap();
        assert_eq!(info.message_count, 2);
    }

    #[test]
    fn delete_conversation_cascades() {
        let (_tmp, store) = temp_store();
        store.get_or_create("conv-5").unwrap();
        store
            .append_message("conv-5", "user", "hello", None)
            .unwrap();

        assert!(store.delete("conv-5").unwrap());
        assert!(store.get("conv-5").unwrap().is_none());
        assert!(!store.delete("conv-5").unwrap()); // already deleted
    }

    #[test]
    fn list_conversations_pagination() {
        let (_tmp, store) = temp_store();
        for i in 0..5 {
            store.get_or_create(&format!("conv-{i}")).unwrap();
        }

        let all = store.list(10, 0).unwrap();
        assert_eq!(all.len(), 5);

        let page = store.list(2, 2).unwrap();
        assert_eq!(page.len(), 2);
    }

    #[test]
    fn tool_calls_json_stored() {
        let (_tmp, store) = temp_store();
        store.get_or_create("conv-6").unwrap();

        let tool_json = r#"[{"name":"shell","input":"ls"}]"#;
        store
            .append_message("conv-6", "assistant", "Here are files", Some(tool_json))
            .unwrap();

        let messages = store.get_recent_messages("conv-6", 10).unwrap();
        assert_eq!(messages[0].tool_calls_json.as_deref(), Some(tool_json));
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let (_tmp, store) = temp_store();
        assert!(store.get("does-not-exist").unwrap().is_none());
    }

    #[test]
    fn max_messages_accessor() {
        let (_tmp, store) = temp_store();
        assert_eq!(store.max_messages(), 50);
    }
}
