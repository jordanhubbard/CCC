//! GET/PUT/DELETE /api/sessions/:key — hub-backed gateway conversation sessions.
//!
//! Allows any fleet agent to load and store Slack/Telegram conversation histories,
//! so session context is preserved regardless of which agent handles each message.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/sessions/:key",
            get(get_session).put(put_session).delete(delete_session),
        )
        .route("/api/sessions", get(list_sessions))
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    let conn = state.fleet_db.lock().await;
    match crate::db::get_session(&conn, &key) {
        Ok(Some(messages)) => Json(json!({"key": key, "messages": messages})).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "session not found"})),
        )
            .into_response(),
        Err(e) => {
            tracing::warn!("get_session error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
struct PutSessionBody {
    agent: Option<String>,
    workspace: Option<String>,
    messages: Vec<Value>,
}

async fn put_session(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(body): Json<PutSessionBody>,
) -> impl IntoResponse {
    let agent = body.agent.as_deref().unwrap_or("");
    let workspace = body.workspace.as_deref().unwrap_or("default");
    let conn = state.fleet_db.lock().await;
    match crate::db::put_session(&conn, &key, agent, workspace, &body.messages) {
        Ok(()) => Json(json!({"ok": true, "key": key})).into_response(),
        Err(e) => {
            tracing::warn!("put_session error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> impl IntoResponse {
    let conn = state.fleet_db.lock().await;
    match crate::db::delete_session(&conn, &key) {
        Ok(()) => Json(json!({"ok": true, "key": key})).into_response(),
        Err(e) => {
            tracing::warn!("delete_session error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

async fn list_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let conn = state.fleet_db.lock().await;
    let result: Result<Vec<Value>, rusqlite::Error> = (|| {
        let mut stmt = conn.prepare_cached(
            "SELECT session_key, agent_name, workspace, updated_at FROM gateway_sessions ORDER BY updated_at DESC LIMIT 500"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(json!({
                "key":        row.get::<_, String>(0)?,
                "agent":      row.get::<_, String>(1)?,
                "workspace":  row.get::<_, String>(2)?,
                "updated_at": row.get::<_, String>(3)?,
            }))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })();
    match result {
        Ok(sessions) => {
            let count = sessions.len();
            Json(json!({"sessions": sessions, "count": count})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
