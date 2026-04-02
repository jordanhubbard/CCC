use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use crate::AppState;
use crate::state::flush_secrets;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/secrets/:key", get(get_secret).post(set_secret))
}

async fn get_secret(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let secrets = state.secrets.read().await;
    match secrets.get(&key) {
        Some(v) => Json(json!({"ok": true, "key": key, "value": v})).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "Secret not found"}))).into_response(),
    }
}

async fn set_secret(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let value = match body.get("value").cloned() {
        Some(v) => v,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "value required"}))).into_response(),
    };
    let mut secrets = state.secrets.write().await;
    secrets.insert(key.clone(), value.clone());
    drop(secrets);
    flush_secrets(&state).await;
    Json(json!({"ok": true, "key": key, "value": value})).into_response()
}
