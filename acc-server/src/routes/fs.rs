/// /api/fs/* — Local-disk AgentFS API
///
/// Reads/writes files under fs_root (ACC_FS_ROOT env var, default /srv/accfs).
/// Replaces the former MinIO/S3-backed implementation.
use crate::AppState;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, head, post},
    Router,
};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/fs/read", get(fs_read))
        .route("/api/fs/write", post(fs_write))
        .route("/api/fs/list", get(fs_list))
        .route("/api/fs/delete", delete(fs_delete))
        .route("/api/fs/exists", head(fs_exists))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fs_root(state: &AppState) -> PathBuf {
    PathBuf::from(&state.fs_root)
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("path required");
    }
    if path.contains("..") || path.starts_with('/') {
        return Err("path traversal not allowed");
    }
    Ok(())
}

fn resolve_full_path(root: &Path, path: &str) -> Result<PathBuf, &'static str> {
    validate_path(path)?;
    Ok(root.join(path))
}

fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("json") => "application/json",
        Some("txt" | "md" | "sh" | "toml" | "yaml" | "yml") => "text/plain; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "application/javascript",
        Some("rs" | "py" | "cpp" | "h" | "c") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// ── GET /api/fs/read?path=... ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct ReadQuery {
    path: String,
    #[serde(default)]
    agent: Option<String>,
}

async fn fs_read(State(state): State<Arc<AppState>>, Query(params): Query<ReadQuery>) -> Response {
    let _ = params.agent; // kept for API compat
    let root = fs_root(&state);
    let full = match resolve_full_path(&root, &params.path) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    };
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let ct = content_type_for(&params.path);
            (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, ct)], bytes).into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "Not found"}))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
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
    let _ = (body.agent, body.scope); // kept for API compat
    let root = fs_root(&state);
    let full = match resolve_full_path(&root, &body.path) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    };
    if let Some(parent) = full.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    }
    let size = body.content.len();
    match tokio::fs::write(&full, body.content.as_bytes()).await {
        Ok(()) => (
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

// ── GET /api/fs/list?prefix=... ───────────────────────────────────────────────

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
    let _ = params.agent;
    let root = fs_root(&state);
    let prefix = params.prefix.unwrap_or_default();

    let scan_root = if prefix.is_empty() {
        root.clone()
    } else {
        if prefix.contains("..") {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "path traversal not allowed"})),
            )
                .into_response();
        }
        root.join(&prefix)
    };

    match walk_dir(&root, &scan_root).await {
        Ok(items) => (StatusCode::OK, Json(json!({"ok": true, "objects": items}))).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::OK, Json(json!({"ok": true, "objects": []}))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn walk_dir(root: &Path, dir: &Path) -> std::io::Result<Vec<serde_json::Value>> {
    let mut results = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                stack.push(path);
            } else {
                let key = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string();
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| {
                        t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| {
                            chrono::DateTime::<chrono::Utc>::from(
                                std::time::UNIX_EPOCH + d,
                            )
                            .to_rfc3339()
                        })
                    })
                    .unwrap_or_default();
                results.push(json!({
                    "key": key,
                    "size": meta.len(),
                    "lastModified": modified,
                }));
            }
        }
    }
    Ok(results)
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
    let root = fs_root(&state);
    let full = match resolve_full_path(&root, &params.path) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    };
    match tokio::fs::remove_file(&full).await {
        Ok(()) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "Not found"}))).into_response()
        }
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
    let root = fs_root(&state);
    let full = match resolve_full_path(&root, &params.path) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    match tokio::fs::metadata(&full).await {
        Ok(_) => StatusCode::OK,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
