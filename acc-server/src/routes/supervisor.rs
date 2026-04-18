use crate::AppState;
use axum::{extract::State, response::Json, routing::get, Router};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/supervisor/status", get(status_handler))
}

async fn status_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    match &state.supervisor {
        Some(handle) => {
            let statuses = handle.statuses.read().await;
            let processes: Vec<Value> = statuses
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "pid": s.pid,
                        "healthy": s.healthy,
                        "restarts": s.restarts,
                        "started_at": s.started_at,
                    })
                })
                .collect();
            Json(json!({"processes": processes}))
        }
        None => Json(json!({"processes": [], "enabled": false})),
    }
}
