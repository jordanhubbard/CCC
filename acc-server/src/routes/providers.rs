use crate::AppState;
/// /routes/providers.rs — List configured infrastructure providers.
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/providers", get(list_providers))
        .route("/api/providers/models", get(list_models))
}

async fn list_providers(State(state): State<Arc<AppState>>) -> Json<Value> {
    let tokenhub_url =
        std::env::var("TOKENHUB_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".to_string());
    let fs_root = std::env::var("ACC_FS_ROOT").unwrap_or_else(|_| "/srv/accfs".to_string());
    let qdrant_url =
        std::env::var("QDRANT_FLEET_URL").unwrap_or_else(|_| "http://127.0.0.1:6333".to_string());

    let supervisor_running = state.supervisor.is_some();

    let providers = vec![
        json!({
            "id":      "tokenhub",
            "kind":    "llm",
            "label":   "TokenHub (LLM Aggregator)",
            "url":     tokenhub_url,
            "status":  "configured",
            "enabled": true,
        }),
        json!({
            "id":      "accfs",
            "kind":    "storage",
            "label":   "AccFS (Samba/SMB)",
            "url":     std::env::var("SMB_HOST").map(|h| format!("smb://{h}/accfs")).unwrap_or_default(),
            "status":  if std::path::Path::new(&fs_root).exists() { "configured" } else { "unconfigured" },
            "enabled": std::path::Path::new(&fs_root).exists(),
        }),
        json!({
            "id":      "qdrant",
            "kind":    "vector",
            "label":   "Qdrant (Vector DB)",
            "url":     qdrant_url,
            "status":  "configured",
            "enabled": true,
        }),
        json!({
            "id":      "supervisor",
            "kind":    "system",
            "label":   "Internal Supervisor",
            "url":     "",
            "status":  if supervisor_running { "running" } else { "disabled" },
            "enabled": supervisor_running,
        }),
    ];

    Json(json!({ "providers": providers }))
}

// ── GET /api/providers/models ─────────────────────────────────────────────
// Proxies tokenhub /v1/models to return available LLM models.

async fn list_models() -> impl IntoResponse {
    let tokenhub_url =
        std::env::var("TOKENHUB_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".to_string());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    match client
        .get(&format!("{}/v1/models", tokenhub_url))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
            Ok(body) => Json(body).into_response(),
            Err(_) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "invalid JSON from tokenhub"})),
            )
                .into_response(),
        },
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            (status, Json(json!({"error": "tokenhub returned error"}))).into_response()
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "tokenhub unreachable", "data": []})),
        )
            .into_response(),
    }
}
