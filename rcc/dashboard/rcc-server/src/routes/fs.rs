use crate::AppState;
use aws_sdk_s3::primitives::ByteStream;
/// /api/fs/* — S3-backed ClawFS API (wq-AGENTFS-001)
///
/// MinIO endpoint from MINIO_ENDPOINT env (default http://localhost:9000).
/// Bucket from MINIO_BUCKET env (default "agents").
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, head, post},
    Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/fs/read", get(fs_read))
        .route("/api/fs/write", post(fs_write))
        .route("/api/fs/list", get(fs_list))
        .route("/api/fs/delete", delete(fs_delete))
        .route("/api/fs/exists", head(fs_exists))
}

// ── Path validation ───────────────────────────────────────────────────────────

fn validate_path(path: &str, agent: Option<&str>) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("path required");
    }
    if path.contains("..") {
        return Err("path traversal not allowed");
    }
    if path.starts_with("shared/") {
        return Ok(());
    }
    if let Some(agent) = agent {
        let prefix = format!("{}/", agent);
        if path.starts_with(&prefix) {
            return Ok(());
        }
    }
    Err("path must start with 'shared/' or '{agent}/'")
}

fn s3_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "S3 not configured"})),
    )
        .into_response()
}

// ── GET /api/fs/read?path=...&agent=... ───────────────────────────────────────

#[derive(Deserialize)]
struct ReadQuery {
    path: String,
    #[serde(default)]
    agent: Option<String>,
}

async fn fs_read(State(state): State<Arc<AppState>>, Query(params): Query<ReadQuery>) -> Response {
    let client = match &state.s3_client {
        Some(c) => c.clone(),
        None => return s3_unavailable(),
    };

    if params.path.contains("..") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "path traversal not allowed"})),
        )
            .into_response();
    }
    if params.path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "path required"})),
        )
            .into_response();
    }

    let result = client
        .get_object()
        .bucket(&state.s3_bucket)
        .key(&params.path)
        .send()
        .await;

    match result {
        Ok(output) => {
            let content_type = output
                .content_type()
                .unwrap_or("application/octet-stream")
                .to_string();
            match output.body.collect().await {
                Ok(data) => {
                    let bytes = data.into_bytes();
                    (
                        StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, content_type)],
                        bytes,
                    )
                        .into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        Err(e) => {
            let is_not_found = e
                .as_service_error()
                .map(|se| se.is_no_such_key())
                .unwrap_or(false);
            if is_not_found {
                (StatusCode::NOT_FOUND, Json(json!({"error": "Not found"}))).into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response()
            }
        }
    }
}

// ── POST /api/fs/write ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WriteBody {
    path: String,
    content: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

async fn fs_write(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WriteBody>,
) -> impl IntoResponse {
    let client = match &state.s3_client {
        Some(c) => c.clone(),
        None => return s3_unavailable(),
    };

    if let Err(e) = validate_path(&body.path, body.agent.as_deref()) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response();
    }

    let _scope = body.scope.as_deref().unwrap_or("private");
    let content_bytes = body.content.into_bytes();
    let size = content_bytes.len();
    let stream = ByteStream::from(content_bytes);

    let result = client
        .put_object()
        .bucket(&state.s3_bucket)
        .key(&body.path)
        .body(stream)
        .send()
        .await;

    match result {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"ok": true, "path": body.path, "size": size})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/fs/list?prefix=...&agent=... ─────────────────────────────────────

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    prefix: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

async fn fs_list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListQuery>,
) -> impl IntoResponse {
    let client = match &state.s3_client {
        Some(c) => c.clone(),
        None => return s3_unavailable(),
    };

    let prefix = params.prefix.unwrap_or_default();

    let mut req = client.list_objects_v2().bucket(&state.s3_bucket);
    if !prefix.is_empty() {
        req = req.prefix(&prefix);
    }

    match req.send().await {
        Ok(output) => {
            let objects: Vec<serde_json::Value> = output
                .contents()
                .iter()
                .map(|obj| {
                    json!({
                        "key": obj.key().unwrap_or(""),
                        "size": obj.size().unwrap_or(0),
                        "lastModified": obj.last_modified()
                            .map(|dt| dt.to_millis().unwrap_or(0))
                            .unwrap_or(0),
                    })
                })
                .collect();
            (
                StatusCode::OK,
                Json(json!({"ok": true, "objects": objects})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── DELETE /api/fs/delete?path=... ────────────────────────────────────────────

#[derive(Deserialize)]
struct DeleteQuery {
    path: String,
}

async fn fs_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<DeleteQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    let client = match &state.s3_client {
        Some(c) => c.clone(),
        None => return s3_unavailable(),
    };

    if params.path.contains("..") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "path traversal not allowed"})),
        )
            .into_response();
    }
    if params.path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "path required"})),
        )
            .into_response();
    }

    match client
        .delete_object()
        .bucket(&state.s3_bucket)
        .key(&params.path)
        .send()
        .await
    {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── HEAD /api/fs/exists?path=... ──────────────────────────────────────────────

#[derive(Deserialize)]
struct ExistsQuery {
    path: String,
}

async fn fs_exists(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ExistsQuery>,
) -> StatusCode {
    let client = match &state.s3_client {
        Some(c) => c.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE,
    };

    if params.path.contains("..") || params.path.is_empty() {
        return StatusCode::BAD_REQUEST;
    }

    match client
        .head_object()
        .bucket(&state.s3_bucket)
        .key(&params.path)
        .send()
        .await
    {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::NOT_FOUND,
    }
}
