use crate::AppState;
/// /routes/acp.rs — ACP (Agent Coding Protocol) session registry.
///
/// Tracks active ACP coding sessions per agent.
/// Agents register/update/remove sessions; dashboard polls for status.
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpSession {
    pub id: String,
    pub agent: String,
    /// "claude-code" | "codex" | "opencode" | "pi"
    pub kind: String,
    pub cwd: Option<String>,
    pub label: Option<String>,
    pub started_at: String,
    pub last_active: String,
    /// "active" | "idle" | "done" | "error"
    pub status: String,
    pub work_item: Option<String>,
}

type AcpMap = RwLock<HashMap<String, HashMap<String, AcpSession>>>;

static ACP_STORE: OnceLock<Arc<AcpMap>> = OnceLock::new();

fn store() -> &'static Arc<AcpMap> {
    ACP_STORE.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/acp/sessions", get(list_all))
        .route("/api/acp/sessions/:agent", get(list_agent).post(register))
        .route("/api/acp/sessions/:agent/:id", delete(remove).put(update))
}

// ── GET /api/acp/sessions ─────────────────────────────────────────────────

async fn list_all() -> impl IntoResponse {
    let data = store().read().await;
    let sessions: Vec<&AcpSession> = data.values().flat_map(|m| m.values()).collect();
    let agent_names: Vec<&String> = data.keys().collect();
    Json(json!({
        "sessions": sessions,
        "total":    sessions.len(),
        "agents":   agent_names,
    }))
}

// ── GET /api/acp/sessions/:agent ──────────────────────────────────────────

async fn list_agent(Path(agent): Path<String>) -> impl IntoResponse {
    let data = store().read().await;
    let sessions: Vec<&AcpSession> = data
        .get(&agent)
        .map(|m| m.values().collect())
        .unwrap_or_default();
    Json(json!({ "agent": agent, "sessions": sessions }))
}

// ── POST /api/acp/sessions/:agent — register a new session ───────────────

async fn register(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(agent): Path<String>,
    Json(body): Json<AcpSession>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let mut data = store().write().await;
    let bucket = data.entry(agent).or_default();
    let id = body.id.clone();
    bucket.insert(id, body);
    (StatusCode::CREATED, Json(json!({"ok": true}))).into_response()
}

// ── DELETE /api/acp/sessions/:agent/:id ──────────────────────────────────

async fn remove(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((agent, id)): Path<(String, String)>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let mut data = store().write().await;
    let removed = data.get_mut(&agent).and_then(|m| m.remove(&id)).is_some();
    if removed {
        Json(json!({"ok": true})).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        )
            .into_response()
    }
}

// ── PUT /api/acp/sessions/:agent/:id — patch status/last_active/label ────

async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((agent, id)): Path<(String, String)>,
    Json(patch): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let mut data = store().write().await;
    match data.get_mut(&agent).and_then(|m| m.get_mut(&id)) {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"session not found"})),
        )
            .into_response(),
        Some(s) => {
            if let Some(v) = patch.get("status").and_then(|v| v.as_str()) {
                s.status = v.to_string();
            }
            if let Some(v) = patch.get("last_active").and_then(|v| v.as_str()) {
                s.last_active = v.to_string();
            }
            if let Some(v) = patch.get("label").and_then(|v| v.as_str()) {
                s.label = Some(v.to_string());
            }
            if let Some(v) = patch.get("work_item").and_then(|v| v.as_str()) {
                s.work_item = Some(v.to_string());
            }
            Json(json!({"ok": true})).into_response()
        }
    }
}
