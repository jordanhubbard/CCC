use crate::brain::BrainRequest;
use crate::AppState;
use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/brain/status", get(brain_status))
        .route("/api/brain/request", post(brain_request))
}

async fn brain_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.brain.status().await)
}

async fn brain_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    let messages = match body.get("messages").and_then(|m| m.as_array()) {
        Some(m) => m.iter().cloned().collect::<Vec<_>>(),
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": "messages array required"})),
            )
                .into_response()
        }
    };

    let id = format!(
        "brain-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        &uuid::Uuid::new_v4().to_string()[..5]
    );

    let req = BrainRequest {
        id: id.clone(),
        messages: messages.into_iter().map(|m| m).collect(),
        max_tokens: body
            .get("maxTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(1024) as u32,
        priority: body
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal")
            .to_string(),
        created: chrono::Utc::now().to_rfc3339(),
        attempts: vec![],
        status: "pending".to_string(),
        result: None,
        completed_at: None,
        callback_url: body
            .get("callbackUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        metadata: body.get("metadata").cloned().unwrap_or(json!({})),
    };

    let request_id = state.brain.enqueue(req).await;

    (
        axum::http::StatusCode::ACCEPTED,
        Json(json!({"ok": true, "requestId": request_id, "status": "queued"})),
    )
        .into_response()
}
