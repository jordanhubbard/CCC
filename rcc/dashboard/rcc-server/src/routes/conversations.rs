/// /api/conversations — File-backed conversation store
///
/// Stored in CONVERSATIONS_PATH (default ./data/conversations.json).
/// Format: array of conversation objects.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::AppState;

static CONVERSATIONS: std::sync::OnceLock<RwLock<Vec<Value>>> = std::sync::OnceLock::new();
static CONVERSATIONS_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn conversations_path() -> &'static str {
    CONVERSATIONS_PATH.get_or_init(|| {
        std::env::var("CONVERSATIONS_PATH")
            .unwrap_or_else(|_| "./data/conversations.json".to_string())
    })
}

fn conversations() -> &'static RwLock<Vec<Value>> {
    CONVERSATIONS.get_or_init(|| RwLock::new(Vec::new()))
}

pub async fn load_conversations() {
    let path = conversations_path();
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            if let Ok(data) = serde_json::from_str::<Vec<Value>>(&content) {
                let count = data.len();
                *conversations().write().await = data;
                tracing::info!("conversations: loaded {} from {}", count, path);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("conversations: failed to load: {}", e),
    }
}

async fn flush_conversations() {
    let data = conversations().read().await;
    let content = match serde_json::to_string_pretty(&*data) {
        Ok(c) => c,
        Err(e) => { tracing::warn!("conversations: serialize failed: {}", e); return; }
    };
    drop(data);
    let path = conversations_path();
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let tmp = format!("{}.tmp", path);
    if let Err(e) = tokio::fs::write(&tmp, &content).await {
        tracing::warn!("conversations: write failed: {}", e);
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        tracing::warn!("conversations: rename failed: {}", e);
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/conversations", get(list_conversations).post(create_conversation))
        .route("/api/conversations/:id", get(get_conversation))
        .route("/api/conversations/:id/messages", post(add_message))
}

// ── GET /api/conversations ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ConversationsQuery {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    since: Option<String>,
}

async fn list_conversations(
    State(_state): State<Arc<AppState>>,
    Query(params): Query<ConversationsQuery>,
) -> impl IntoResponse {
    let convs = conversations().read().await;
    let mut result: Vec<Value> = convs.iter().cloned().collect();
    drop(convs);

    if let Some(project) = &params.project {
        result.retain(|c| c.get("projectId").and_then(|v| v.as_str()) == Some(project.as_str()));
    }
    if let Some(agent) = &params.agent {
        result.retain(|c| {
            c.get("participants")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().any(|a| a.as_str() == Some(agent.as_str())))
                .unwrap_or(false)
        });
    }
    if let Some(channel) = &params.channel {
        result.retain(|c| c.get("channel").and_then(|v| v.as_str()) == Some(channel.as_str()));
    }
    if let Some(since) = &params.since {
        result.retain(|c| {
            c.get("createdAt")
                .and_then(|v| v.as_str())
                .map(|ts| ts >= since.as_str())
                .unwrap_or(false)
        });
    }

    Json(result).into_response()
}

// ── POST /api/conversations ───────────────────────────────────────────────────

async fn create_conversation(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let now = chrono::Utc::now().to_rfc3339();
    let conv = json!({
        "id": format!("conv-{}", chrono::Utc::now().timestamp_millis()),
        "participants": body.get("participants").cloned().unwrap_or(json!([])),
        "channel": body.get("channel").cloned().unwrap_or(Value::Null),
        "projectId": body.get("projectId").cloned().unwrap_or(Value::Null),
        "messages": body.get("messages").cloned().unwrap_or(json!([])),
        "tags": body.get("tags").cloned().unwrap_or(json!([])),
        "createdAt": now,
        "updatedAt": now,
    });

    conversations().write().await.push(conv.clone());
    flush_conversations().await;

    (StatusCode::CREATED, Json(json!({"ok": true, "conversation": conv}))).into_response()
}

// ── GET /api/conversations/:id ────────────────────────────────────────────────

async fn get_conversation(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let convs = conversations().read().await;
    let conv = convs.iter().find(|c| c.get("id").and_then(|v| v.as_str()) == Some(&id)).cloned();
    drop(convs);

    match conv {
        Some(c) => Json(c).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "Conversation not found"}))).into_response(),
    }
}

// ── POST /api/conversations/:id/messages ─────────────────────────────────────

async fn add_message(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let author = match body.get("author").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "author and text required"}))).into_response(),
    };
    let text = match body.get("text").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "author and text required"}))).into_response(),
    };

    let message = json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "author": author,
        "text": text,
    });

    {
        let mut convs = conversations().write().await;
        let idx = convs.iter().position(|c| c.get("id").and_then(|v| v.as_str()) == Some(&id));
        match idx {
            None => return (StatusCode::NOT_FOUND, Json(json!({"error": "Conversation not found"}))).into_response(),
            Some(i) => {
                if let Some(obj) = convs[i].as_object_mut() {
                    let messages = obj.entry("messages").or_insert_with(|| json!([]));
                    if let Some(arr) = messages.as_array_mut() {
                        arr.push(message.clone());
                    }
                    obj.insert("updatedAt".to_string(), json!(chrono::Utc::now().to_rfc3339()));
                }
            }
        }
    }

    flush_conversations().await;

    (StatusCode::CREATED, Json(json!({"ok": true, "message": message}))).into_response()
}
