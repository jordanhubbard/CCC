use crate::state::flush_secrets;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/secrets", get(list_secrets))
        .route("/api/secrets/:key", get(get_secret).post(set_secret).put(set_secret).delete(delete_secret))
}

async fn list_secrets(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    let secrets = state.secrets.read().await;
    let keys: Vec<&str> = secrets.keys().map(|k| k.as_str()).collect();
    Json(json!({"ok": true, "keys": keys, "count": keys.len()})).into_response()
}

async fn get_secret(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    let secrets = state.secrets.read().await;
    match secrets.get(&key) {
        Some(v) => Json(json!({"ok": true, "key": key, "value": v})).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Secret not found"})),
        )
            .into_response(),
    }
}

async fn set_secret(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    let value = match body.get("value").cloned() {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "value required"})),
            )
                .into_response()
        }
    };
    let mut secrets = state.secrets.write().await;
    secrets.insert(key.clone(), value.clone());
    drop(secrets);
    flush_secrets(&state).await;
    Json(json!({"ok": true, "key": key, "value": value})).into_response()
}

async fn delete_secret(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    let mut secrets = state.secrets.write().await;
    if secrets.remove(&key).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Secret not found"})),
        )
            .into_response();
    }
    drop(secrets);
    flush_secrets(&state).await;
    Json(json!({"ok": true, "key": key, "deleted": true})).into_response()
}
