use crate::AppState;
/// /routes/setup.rs — CCC setup/config API and status endpoint.
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/setup/status", get(get_setup_status))
        .route("/api/setup/config", get(get_config).put(put_config))
}

// ── GET /api/setup/status ─────────────────────────────────────────────────

async fn get_setup_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let first_run = state.auth_tokens.is_empty();

    let tokenhub_url = std::env::var("TOKENHUB_URL").unwrap_or_default();
    let has_tokenhub = !tokenhub_url.is_empty()
        && tokenhub_url != "http://127.0.0.1:8090"  // non-default
        || !tokenhub_url.is_empty(); // any configured value counts

    let has_minio = !std::env::var("MINIO_ENDPOINT")
        .unwrap_or_default()
        .is_empty();

    let agent_name = std::env::var("AGENT_NAME").unwrap_or_else(|_| "rcc".to_string());

    Json(json!({
        "first_run": first_run,
        "has_tokenhub": !tokenhub_url.is_empty(),
        "has_minio": has_minio,
        "agent_name": agent_name,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ── GET /api/setup/config ─────────────────────────────────────────────────

async fn get_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }

    let port: u16 = std::env::var("RCC_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8789);

    Json(json!({
        "agent_name":          std::env::var("AGENT_NAME").unwrap_or_else(|_| "rcc".to_string()),
        "public_url":          std::env::var("RCC_HOST_PUBLIC").unwrap_or_default(),
        "tokenhub_url":        std::env::var("TOKENHUB_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".to_string()),
        "supervisor_enabled":  std::env::var("SUPERVISOR_ENABLED").unwrap_or_default() == "true",
        "minio_endpoint":      std::env::var("MINIO_ENDPOINT").unwrap_or_default(),
        "minio_bucket":        std::env::var("MINIO_BUCKET").unwrap_or_else(|_| "agents".to_string()),
        "rcc_port":            port,
        "log_level":           std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        "crush_server_url":    std::env::var("CRUSH_SERVER_URL").unwrap_or_else(|_| "http://localhost:8795".to_string()),
        "sc_url":              std::env::var("SC_URL").unwrap_or_else(|_| "http://localhost:8793".to_string()),
    })).into_response()
}

// ── PUT /api/setup/config ─────────────────────────────────────────────────
// Runtime-only: applies env vars for the current process lifetime.
// Does not persist to disk (Phase 2 will add rcc.json config file).

async fn put_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }

    let obj = match body.as_object() {
        Some(o) => o,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"body must be a JSON object"})),
            )
                .into_response()
        }
    };

    let allowed = [
        ("agent_name", "AGENT_NAME"),
        ("public_url", "RCC_HOST_PUBLIC"),
        ("tokenhub_url", "TOKENHUB_URL"),
        ("minio_endpoint", "MINIO_ENDPOINT"),
        ("minio_bucket", "MINIO_BUCKET"),
        ("log_level", "RUST_LOG"),
        ("crush_server_url", "CRUSH_SERVER_URL"),
        ("sc_url", "SC_URL"),
    ];

    let mut applied = Vec::new();
    for (key, env_key) in &allowed {
        if let Some(val) = obj.get(*key).and_then(|v| v.as_str()) {
            // SAFETY: single-threaded setup context; env mutation is a known tradeoff
            unsafe { std::env::set_var(env_key, val) };
            applied.push(*key);
        }
    }

    // supervisor_enabled is bool — handle separately
    if let Some(v) = obj.get("supervisor_enabled").and_then(|v| v.as_bool()) {
        unsafe { std::env::set_var("SUPERVISOR_ENABLED", if v { "true" } else { "false" }) };
        applied.push("supervisor_enabled");
    }

    Json(json!({
        "ok": true,
        "applied": applied,
        "note": "Runtime-only. Restart rcc-server or persist to .env for permanence."
    }))
    .into_response()
}
