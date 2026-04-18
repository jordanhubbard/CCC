use crate::AppState;
use axum::{extract::State, response::Json, routing::get, Router};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // Both endpoints are intentionally unauthenticated.
        // /api/health is a liveness probe — must respond even if auth is misconfigured.
        // /api/status is a readiness probe used by load balancers, Consul, and the dashboard.
        // Neither endpoint exposes sensitive data; they return structural metrics only.
        // Auth is enforced on all mutating endpoints (/api/queue, /api/agents, etc.).
        .route("/api/health", get(health_handler))
        .route("/api/status", get(status_handler))
}

async fn health_handler() -> Json<Value> {
    Json(json!({"ok": true, "service": "acc-server"}))
}

async fn status_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let uptime_secs = state.start_time.elapsed().unwrap_or_default().as_secs();
    let queue = state.queue.read().await;
    let agents = state.agents.read().await;
    let agent_count = agents.as_object().map(|m| m.len()).unwrap_or(0);
    let pending = queue
        .items
        .iter()
        .filter(|i| i.get("status").and_then(|s| s.as_str()) == Some("pending"))
        .count();
    let in_progress = queue
        .items
        .iter()
        .filter(|i| i.get("status").and_then(|s| s.as_str()) == Some("in-progress"))
        .count();
    Json(json!({
        "ok": true,
        "service": "acc-server",
        "uptime_secs": uptime_secs,
        "queue": {
            "pending": pending,
            "in_progress": in_progress,
            "total": queue.items.len(),
            "completed": queue.completed.len()
        },
        "agents": agent_count
    }))
}
