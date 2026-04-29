use crate::{
    bus_types::{BlobMeta, DlqEntry, MediaType},
    AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/bus/blobs/upload", post(blob_upload))
        .route("/api/bus/blobs", get(blob_list))
        .route("/api/bus/blobs/:id", get(blob_meta))
        .route("/api/bus/blobs/:id", delete(blob_delete))
        .route("/api/bus/blobs/:id/download", get(blob_download))
        .route("/api/bus/dlq", get(dlq_list).post(dlq_append))
        .route("/api/bus/dlq/redeliver", post(dlq_redeliver))
}

// ── Upload ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct UploadBody {
    blob_id: Option<String>,
    chunk_index: Option<u64>,
    total_chunks: Option<u64>,
    mime_type: Option<String>,
    enc: Option<String>,
    data: Option<String>,
    ttl_seconds: Option<u64>,
    allowed_agents: Option<Vec<String>>,
}

async fn blob_upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<UploadBody>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }

    let mime_str = match body.mime_type {
        Some(m) => m,
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"mime_type required"})),
            )
                .into_response()
        }
    };
    let mime: MediaType = mime_str.parse().unwrap_or_else(|_| unreachable!());
    if !mime.is_known() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "unknown_media_type",
                "mime_type": mime_str,
                "known_types": MediaType::all_known(),
            })),
        )
            .into_response();
    }

    let enc = body.enc.as_deref().unwrap_or("none");
    if mime.is_binary() && enc != "base64" {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "binary_type_requires_base64_enc",
                "mime_type": mime_str,
            })),
        )
            .into_response();
    }

    let data_str = match body.data {
        Some(d) => d,
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"data required"})),
            )
                .into_response()
        }
    };

    let chunk_index = body.chunk_index.unwrap_or(0) as usize;
    let total_chunks = body.total_chunks.unwrap_or(1) as usize;

    let chunk_bytes: Vec<u8> = if enc == "base64" {
        match b64_decode(&data_str) {
            Ok(b) => b,
            Err(_) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error":"invalid_base64"})),
                )
                    .into_response()
            }
        }
    } else {
        data_str.into_bytes()
    };

    let blob_id = body
        .blob_id
        .unwrap_or_else(|| format!("blob-{}", uuid::Uuid::new_v4()));
    let ttl_s = body.ttl_seconds.unwrap_or(86400);
    let allowed = body.allowed_agents.unwrap_or_default();
    let uploaded_by = extract_token(&headers);

    // Write chunk to disk
    let blob_dir = std::path::Path::new(&state.blobs_path).join(&blob_id);
    let data_path = blob_dir.join("data");
    if let Err(e) = tokio::fs::create_dir_all(&blob_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("storage: {e}")})),
        )
            .into_response();
    }

    if chunk_index == 0 {
        if let Err(e) = tokio::fs::write(&data_path, &chunk_bytes).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("write: {e}")})),
            )
                .into_response();
        }
    } else {
        let mut f = match tokio::fs::OpenOptions::new()
            .append(true)
            .open(&data_path)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("append open: {e}")})),
                )
                    .into_response()
            }
        };
        if let Err(e) = f.write_all(&chunk_bytes).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("append write: {e}")})),
            )
                .into_response();
        }
    }

    let now = chrono::Utc::now();
    let expires_at = if ttl_s > 0 {
        Some((now + chrono::Duration::seconds(ttl_s as i64)).to_rfc3339())
    } else {
        None
    };

    let (complete, meta) = {
        let mut store = state.blob_store.write().await;
        let entry = store.entry(blob_id.clone()).or_insert_with(|| BlobMeta {
            id: blob_id.clone(),
            mime_type: mime.clone(),
            size_bytes: 0,
            uploaded_by: uploaded_by.clone(),
            uploaded_at: now.to_rfc3339(),
            expires_at: expires_at.clone(),
            allowed_agents: allowed,
            total_chunks,
            chunks_received: 0,
            complete: false,
        });
        entry.chunks_received += 1;
        entry.size_bytes += chunk_bytes.len() as u64;
        let complete = entry.chunks_received >= entry.total_chunks;
        entry.complete = complete;
        (complete, entry.clone())
    };

    if complete {
        let seq = state.bus_seq.fetch_add(1, Ordering::SeqCst);
        let event = serde_json::to_string(&json!({
            "id": format!("msg-{seq}"),
            "seq": seq,
            "ts": now.to_rfc3339(),
            "type": "bus.blob_ready",
            "blob_id": blob_id,
            "mime_type": mime_str,
            "size_bytes": meta.size_bytes,
        }))
        .unwrap_or_default();
        let _ = state.bus_tx.send(event.clone());
        let _ = append_line(&state.bus_log_path, &format!("{}\n", event)).await;
    }

    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "blob_id": blob_id,
            "complete": complete,
            "chunks_received": meta.chunks_received,
            "total_chunks": total_chunks,
        })),
    )
        .into_response()
}

// ── List ──────────────────────────────────────────────────────────────────────

async fn blob_list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let now = chrono::Utc::now();
    let store = state.blob_store.read().await;
    let blobs: Vec<Value> = store
        .values()
        .filter(|m| {
            m.expires_at
                .as_ref()
                .and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok())
                .map(|e| e > now)
                .unwrap_or(true)
        })
        .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
        .collect();
    Json(json!({"blobs": blobs})).into_response()
}

// ── Meta ──────────────────────────────────────────────────────────────────────

async fn blob_meta(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(blob_id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let store = state.blob_store.read().await;
    match store.get(&blob_id) {
        Some(m) => (
            StatusCode::OK,
            Json(serde_json::to_value(m).unwrap_or(Value::Null)),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error":"not_found"}))).into_response(),
    }
}

// ── Delete ────────────────────────────────────────────────────────────────────

async fn blob_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(blob_id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let removed = state.blob_store.write().await.remove(&blob_id).is_some();
    if removed {
        let path = std::path::Path::new(&state.blobs_path).join(&blob_id);
        let _ = tokio::fs::remove_dir_all(&path).await;
        (StatusCode::OK, Json(json!({"ok":true}))).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error":"not_found"}))).into_response()
    }
}

// ── Download ──────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct DownloadQuery {
    agent: Option<String>,
}

async fn blob_download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(blob_id): Path<String>,
    Query(q): Query<DownloadQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }

    // Lazy TTL expiry
    {
        let mut store = state.blob_store.write().await;
        if let Some(meta) = store.get(&blob_id) {
            if let Some(exp_str) = &meta.expires_at {
                if let Ok(exp) = chrono::DateTime::parse_from_rfc3339(exp_str) {
                    if exp < chrono::Utc::now() {
                        let path = std::path::Path::new(&state.blobs_path).join(&blob_id);
                        let _ = tokio::fs::remove_dir_all(&path).await;
                        store.remove(&blob_id);
                        return (StatusCode::GONE, Json(json!({"error":"blob_expired"})))
                            .into_response();
                    }
                }
            }
        }
    }

    let meta = {
        let store = state.blob_store.read().await;
        match store.get(&blob_id) {
            Some(m) if m.complete => m.clone(),
            Some(_) => {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({"error":"upload_incomplete"})),
                )
                    .into_response()
            }
            None => {
                return (StatusCode::NOT_FOUND, Json(json!({"error":"not_found"}))).into_response()
            }
        }
    };

    // Access control: empty allowed_agents = public
    if !meta.allowed_agents.is_empty() {
        let req_agent = q.agent.as_deref().unwrap_or("");
        if !meta.allowed_agents.iter().any(|a| a == req_agent) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error":"access_denied"})),
            )
                .into_response();
        }
    }

    let data_path = std::path::Path::new(&state.blobs_path)
        .join(&blob_id)
        .join("data");

    match tokio::fs::read(&data_path).await {
        Ok(bytes) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "blob_id": blob_id,
                "mime_type": meta.mime_type.as_str(),
                "enc": "base64",
                "size_bytes": meta.size_bytes,
                "data": b64_encode(&bytes),
            })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"data_not_found"})),
        )
            .into_response(),
    }
}

// ── DLQ: append ──────────────────────────────────────────────────────────────

async fn dlq_append(
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
    let entry = DlqEntry {
        id: format!("dlq-{}", uuid::Uuid::new_v4()),
        ts: chrono::Utc::now().to_rfc3339(),
        error: body
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown")
            .to_string(),
        message: body.get("message").cloned().unwrap_or(body.clone()),
        retry_count: 0,
    };
    let line = format!("{}\n", serde_json::to_string(&entry).unwrap_or_default());
    if let Err(e) = append_line(&state.dlq_path, &line).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("dlq write: {e}")})),
        )
            .into_response();
    }
    (StatusCode::OK, Json(json!({"ok":true,"id":entry.id}))).into_response()
}

// ── DLQ: list ─────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct DlqQuery {
    max_age_seconds: Option<u64>,
}

async fn dlq_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<DlqQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let content = tokio::fs::read_to_string(&state.dlq_path)
        .await
        .unwrap_or_default();
    let now = chrono::Utc::now();
    let entries: Vec<Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<DlqEntry>(l).ok())
        .filter(|e| {
            if let Some(max_age) = q.max_age_seconds {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&e.ts) {
                    return (now - ts.with_timezone(&chrono::Utc)).num_seconds() <= max_age as i64;
                }
            }
            true
        })
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    Json(json!({"entries": entries})).into_response()
}

// ── DLQ: redeliver ────────────────────────────────────────────────────────────

async fn dlq_redeliver(
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
    let dlq_id = match body.get("dlq_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"dlq_id required"})),
            )
                .into_response()
        }
    };

    // Find entry in DLQ file
    let content = tokio::fs::read_to_string(&state.dlq_path)
        .await
        .unwrap_or_default();
    let entry: Option<DlqEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<DlqEntry>(l).ok())
        .find(|e| e.id == dlq_id);

    let entry = match entry {
        Some(e) => e,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"dlq_entry_not_found"})),
            )
                .into_response()
        }
    };

    // Re-send message to bus
    let mut msg = entry.message.clone();
    let seq = state.bus_seq.fetch_add(1, Ordering::SeqCst);
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(obj) = msg.as_object_mut() {
        obj.insert("id".into(), json!(format!("msg-{seq}")));
        obj.insert("seq".into(), json!(seq));
        obj.insert("ts".into(), json!(now));
        obj.insert("dlq_redelivered".into(), json!(true));
        obj.insert("dlq_id".into(), json!(dlq_id));
    }
    let msg_str = serde_json::to_string(&msg).unwrap_or_default();
    let _ = state.bus_tx.send(msg_str.clone());
    let _ = append_line(&state.bus_log_path, &format!("{}\n", msg_str)).await;

    (
        StatusCode::OK,
        Json(json!({"ok":true,"redelivered":true,"seq":seq})),
    )
        .into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn b64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s)
}

fn extract_token(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim_start_matches("Bearer ")
        .trim()
        .to_string()
}

async fn append_line(path: &str, line: &str) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    f.write_all(line.as_bytes()).await
}
