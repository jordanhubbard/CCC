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
    let fs_root = std::env::var("ACC_FS_ROOT").unwrap_or_else(|_| "/srv/accfs".to_string());
    let has_accfs = std::path::Path::new(&fs_root).exists();
    let agent_name = std::env::var("AGENT_NAME").unwrap_or_else(|_| "ccc".to_string());

    Json(json!({
        "first_run": first_run,
        "has_tokenhub": !tokenhub_url.is_empty(),
        "has_accfs": has_accfs,
        "agent_name": agent_name,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ── GET /api/setup/config ─────────────────────────────────────────────────

async fn get_config(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    let port: u16 = std::env::var("ACC_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8789);

    Json(json!({
        "agent_name":         std::env::var("AGENT_NAME").unwrap_or_else(|_| "ccc".to_string()),
        "public_url":         std::env::var("ACC_HOST_PUBLIC").unwrap_or_default(),
        "tokenhub_url":       std::env::var("TOKENHUB_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".to_string()),
        "supervisor_enabled": std::env::var("SUPERVISOR_ENABLED").unwrap_or_default() == "true",
        "fs_root":            std::env::var("ACC_FS_ROOT").unwrap_or_else(|_| "/srv/accfs".to_string()),
        "ccc_port":           port,
        "log_level":          std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
    })).into_response()
}

// ── PUT /api/setup/config ─────────────────────────────────────────────────
// Runtime-only: writes to process env for the lifetime of the process.
// NOTE: env mutation is not thread-safe in Rust. This endpoint must only be
// called during bootstrap, before agents start making concurrent requests.
// Phase 2 will replace this with a proper ccc.json config file + reload.

async fn put_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
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
        ("agent_name",    "AGENT_NAME"),
        ("public_url",    "ACC_HOST_PUBLIC"),
        ("tokenhub_url",  "TOKENHUB_URL"),
        ("fs_root",       "ACC_FS_ROOT"),
        ("log_level",     "RUST_LOG"),
    ];

    let mut applied = Vec::new();
    for (key, env_key) in &allowed {
        if let Some(val) = obj.get(*key).and_then(|v| v.as_str()) {
            // Safety: bootstrap-only endpoint called before concurrent request load.
            // This is a known limitation; Phase 2 replaces this with ccc.json writes.
            #[allow(unused_unsafe)]
            unsafe { std::env::set_var(env_key, val) };
            applied.push(*key);
        }
    }

    if let Some(v) = obj.get("supervisor_enabled").and_then(|v| v.as_bool()) {
        #[allow(unused_unsafe)]
        unsafe { std::env::set_var("SUPERVISOR_ENABLED", if v { "true" } else { "false" }) };
        applied.push("supervisor_enabled");
    }

    Json(json!({
        "ok": true,
        "applied": applied,
        "note": "Runtime-only. Restart acc-server or persist to .env for permanence."
    }))
    .into_response()
}
