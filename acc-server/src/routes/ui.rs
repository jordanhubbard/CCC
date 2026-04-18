use crate::AppState;
/// /routes/ui.rs — bootstrap token issuer and grievances proxy.
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

fn grievances_url() -> String {
    std::env::var("GRIEVANCES_URL").unwrap_or_else(|_| "http://localhost:9999".to_string())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // Bootstrap
        .route("/api/bootstrap", get(get_bootstrap))
        .route("/api/bootstrap/token", post(post_bootstrap_token))
        // Grievances proxy
        .route("/grievances", get(proxy_grievances_root))
        .route("/grievances/*path", get(proxy_grievances))
        .route("/api/grievances", get(proxy_api_grievances_root))
        .route("/api/grievances/*path", get(proxy_api_grievances))
}

// ── /api/bootstrap ────────────────────────────────────────────────────────

async fn get_bootstrap(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = params.get("token").cloned().unwrap_or_default();
    // Check if token matches any agent's bootstrap token
    let agents = state.agents.read().await;
    let secrets = state.secrets.read().await;

    let global_bootstrap = secrets
        .get("bootstrap/token")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !global_bootstrap.is_empty() && token == global_bootstrap {
        return Json(json!({
            "ok": true,
            "ccc_url": format!("http://{}:{}", 
                std::env::var("PUBLIC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
                std::env::var("ACC_PORT").unwrap_or_else(|_| "8789".to_string())
            ),
            "tokenhub_url": std::env::var("TOKENHUB_URL").unwrap_or_else(|_| "http://127.0.0.1:8090".to_string()),
        })).into_response();
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "Invalid bootstrap token"})),
    )
        .into_response()
}

async fn post_bootstrap_token(
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
    let agent_name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"name required"})),
            )
                .into_response()
        }
    };
    let token = format!(
        "bt-{}-{}",
        agent_name,
        uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string()
    );
    let mut secrets = state.secrets.write().await;
    secrets.insert(format!("bootstrap/{}", agent_name), json!(token));
    drop(secrets);
    crate::state::flush_secrets(&state).await;
    (
        StatusCode::CREATED,
        Json(json!({"ok": true, "token": token, "agent": agent_name})),
    )
        .into_response()
}

// ── Grievances proxy ──────────────────────────────────────────────────────

async fn proxy_grievances_root() -> impl IntoResponse {
    proxy_to_grievances("/").await
}

async fn proxy_grievances(Path(path): Path<String>) -> impl IntoResponse {
    proxy_to_grievances(&format!("/{}", path)).await
}

async fn proxy_api_grievances_root() -> impl IntoResponse {
    proxy_to_grievances("/api").await
}

async fn proxy_api_grievances(Path(path): Path<String>) -> impl IntoResponse {
    proxy_to_grievances(&format!("/api/{}", path)).await
}

async fn proxy_to_grievances(path: &str) -> impl IntoResponse {
    let url = format!("{}{}", grievances_url(), path);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    match client.get(&url).send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let body = resp.text().await.unwrap_or_default();
            (status, body).into_response()
        }
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            "Grievance server unavailable. The irony is noted.",
        )
            .into_response(),
    }
}
