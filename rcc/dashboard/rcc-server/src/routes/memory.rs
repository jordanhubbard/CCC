/// /api/memory/* and /api/vector/* — Milvus vector memory (SOA-007)
///
/// Embeddings via tokenhub at http://127.0.0.1:8090/v1/embeddings (text-embedding-3-large)
/// Milvus REST API v2 at http://100.89.199.14:19530

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use crate::AppState;

const MILVUS_BASE: &str = "http://100.89.199.14:19530";
const TOKENHUB_BASE: &str = "http://127.0.0.1:8090";
const EMBED_MODEL: &str = "text-embedding-3-large";
const RCC_MEMORY_COLLECTION: &str = "rcc_memory";

static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build memory HTTP client")
    })
}

// ── Embedding helper ──────────────────────────────────────────────────────────

async fn embed(text: &str) -> Result<Vec<f64>, String> {
    let resp = http_client()
        .post(format!("{}/v1/embeddings", TOKENHUB_BASE))
        .json(&json!({ "model": EMBED_MODEL, "input": text }))
        .send()
        .await
        .map_err(|e| format!("tokenhub request failed: {}", e))?;

    let body: Value = resp.json().await
        .map_err(|e| format!("tokenhub response parse failed: {}", e))?;

    body["data"][0]["embedding"]
        .as_array()
        .ok_or_else(|| format!("no embedding in tokenhub response: {:?}", body))?
        .iter()
        .map(|v| v.as_f64().ok_or_else(|| "non-float in embedding".to_string()))
        .collect()
}

// ── Content-addressed ID ──────────────────────────────────────────────────────

fn content_id(agent: &str, text: &str) -> String {
    let prefix: String = text.chars().take(128).collect();
    let input = format!("{}:{}", agent, prefix);
    hex::encode(Sha256::digest(input.as_bytes()))
}

// ── Milvus helpers ────────────────────────────────────────────────────────────

async fn milvus_upsert(collection: &str, record: Value) -> Result<(), String> {
    let resp = http_client()
        .post(format!("{}/v2/vectordb/entities/upsert", MILVUS_BASE))
        .json(&json!({ "collectionName": collection, "data": [record] }))
        .send()
        .await
        .map_err(|e| format!("milvus upsert request failed: {}", e))?;

    let body: Value = resp.json().await
        .map_err(|e| format!("milvus upsert response parse failed: {}", e))?;

    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        return Err(format!("milvus upsert error {}: {:?}", code, body.get("message")));
    }
    Ok(())
}

async fn milvus_search(
    collection: &str,
    vector: Vec<f64>,
    limit: usize,
    filter: Option<&str>,
    output_fields: &[&str],
) -> Result<Vec<Value>, String> {
    let mut req = json!({
        "collectionName": collection,
        "data": [vector],
        "limit": limit,
        "outputFields": output_fields,
    });
    if let Some(f) = filter {
        req["filter"] = json!(f);
    }

    let resp = http_client()
        .post(format!("{}/v2/vectordb/entities/search", MILVUS_BASE))
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("milvus search request failed: {}", e))?;

    let body: Value = resp.json().await
        .map_err(|e| format!("milvus search response parse failed: {}", e))?;

    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        return Err(format!("milvus search error {}: {:?}", code, body.get("message")));
    }

    Ok(body["data"].as_array().cloned().unwrap_or_default())
}

async fn milvus_list_collections() -> Result<Vec<Value>, String> {
    let resp = http_client()
        .post(format!("{}/v2/vectordb/collections/list", MILVUS_BASE))
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| format!("milvus list request failed: {}", e))?;

    let body: Value = resp.json().await
        .map_err(|e| format!("milvus list response parse failed: {}", e))?;

    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        return Err(format!("milvus list error {}: {:?}", code, body.get("message")));
    }

    Ok(body["data"].as_array().cloned().unwrap_or_default())
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/memory/ingest", post(memory_ingest))
        .route("/api/memory/recall", get(memory_recall))
        .route("/api/vector/health", get(vector_health))
        .route("/api/vector/search", get(vector_search))
        .route("/api/vector/upsert", post(vector_upsert))
}

// ── POST /api/memory/ingest ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct IngestBody {
    text: String,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    conv_id: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

async fn memory_ingest(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<IngestBody>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    if body.text.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "text required"}))).into_response();
    }

    let agent = body.agent.as_deref().unwrap_or("unknown");
    let id = content_id(agent, &body.text);
    let source = body.source.as_deref().unwrap_or("api");
    let ts = chrono::Utc::now().timestamp_millis();

    // Log optional scope fields for tracing; not stored in rcc_memory schema
    tracing::debug!(
        platform = ?body.platform,
        workspace_id = ?body.workspace_id,
        channel_id = ?body.channel_id,
        user_id = ?body.user_id,
        conv_id = ?body.conv_id,
        "memory ingest scope"
    );

    let vector = match embed(&body.text).await {
        Ok(v) => v,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    };

    let content: String = body.text.chars().take(4096).collect();
    let agent_trunc: String = agent.chars().take(32).collect();
    let source_trunc: String = source.chars().take(64).collect();
    let id_trunc: String = id.chars().take(128).collect();

    let record = json!({
        "id":      id_trunc,
        "vector":  vector,
        "agent":   agent_trunc,
        "content": content,
        "source":  source_trunc,
        "ts":      ts,
    });

    match milvus_upsert(RCC_MEMORY_COLLECTION, record).await {
        Ok(_) => (axum::http::StatusCode::OK, Json(json!({"ok": true, "id": id_trunc}))).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

// ── GET /api/memory/recall ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RecallQuery {
    q: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    k: Option<usize>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
}

async fn memory_recall(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<RecallQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    if params.q.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "Missing query parameter q"}))).into_response();
    }

    let vector = match embed(&params.q).await {
        Ok(v) => v,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    };

    let k = params.k.unwrap_or(8);

    // Build Milvus filter from available schema fields (agent only; platform/channel_id not in schema)
    let mut filter_parts: Vec<String> = Vec::new();
    if let Some(agent) = &params.agent {
        if !agent.is_empty() {
            filter_parts.push(format!("agent == \"{}\"", agent.replace('"', "\\\"")));
        }
    }
    // Log scope params for debugging even if not filterable
    tracing::debug!(platform = ?params.platform, channel_id = ?params.channel_id, "memory recall scope");
    let filter = if filter_parts.is_empty() { None } else { Some(filter_parts.join(" && ")) };

    let raw = match milvus_search(
        RCC_MEMORY_COLLECTION,
        vector,
        k,
        filter.as_deref(),
        &["id", "content", "agent", "source", "ts"],
    ).await {
        Ok(r) => r,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    };

    let results: Vec<Value> = raw.into_iter().map(|r| json!({
        "id":      r.get("id").cloned().unwrap_or(Value::Null),
        "content": r.get("content").cloned().unwrap_or(Value::Null),
        "agent":   r.get("agent").cloned().unwrap_or(Value::Null),
        "source":  r.get("source").cloned().unwrap_or(Value::Null),
        "score":   r.get("distance").cloned().unwrap_or(Value::Null),
        "ts":      r.get("ts").cloned().unwrap_or(Value::Null),
    })).collect();

    (axum::http::StatusCode::OK, Json(json!({"ok": true, "results": results}))).into_response()
}

// ── GET /api/vector/health ────────────────────────────────────────────────────

async fn vector_health() -> impl IntoResponse {
    match milvus_list_collections().await {
        Ok(collections) => (axum::http::StatusCode::OK, Json(json!({"ok": true, "collections": collections}))).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}

// ── GET /api/vector/search ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VectorSearchQuery {
    q: String,
    #[serde(default)]
    k: Option<usize>,
    #[serde(default)]
    collections: Option<String>,
}

async fn vector_search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<VectorSearchQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    if params.q.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "Missing query parameter q"}))).into_response();
    }

    let vector = match embed(&params.q).await {
        Ok(v) => v,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    };

    let k = params.k.unwrap_or(10);
    let collections_param = params.collections.as_deref().unwrap_or("all");

    let target_collections: Vec<String> = if collections_param == "all" {
        match milvus_list_collections().await {
            Ok(cols) => cols.iter()
                .filter_map(|c| c.as_str().map(|s| s.to_string()))
                .collect(),
            Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
        }
    } else {
        collections_param.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    };

    let mut all_results: Vec<Value> = Vec::new();
    for col in &target_collections {
        match milvus_search(col, vector.clone(), k, None, &["id", "content", "agent", "source", "ts"]).await {
            Ok(hits) => {
                for mut hit in hits {
                    if let Some(obj) = hit.as_object_mut() {
                        obj.insert("collection".to_string(), json!(col));
                        if let Some(dist) = obj.remove("distance") {
                            obj.insert("score".to_string(), dist);
                        }
                    }
                    all_results.push(hit);
                }
            }
            Err(e) => tracing::warn!("vector search on collection {}: {}", col, e),
        }
    }

    all_results.sort_by(|a, b| {
        let sa = a.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let sb = b.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    all_results.truncate(k);

    (axum::http::StatusCode::OK, Json(json!({"ok": true, "results": all_results}))).into_response()
}

// ── POST /api/vector/upsert ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct VectorUpsertBody {
    collection: String,
    id: String,
    text: String,
    #[serde(default)]
    metadata: Option<Value>,
}

async fn vector_upsert(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<VectorUpsertBody>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    if body.collection.is_empty() || body.id.is_empty() || body.text.is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "Missing required fields: collection, id, text"}))).into_response();
    }

    let vector = match embed(&body.text).await {
        Ok(v) => v,
        Err(e) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    };

    let mut record = json!({
        "id":     body.id,
        "vector": vector,
        "content": body.text,
    });

    if let Some(meta) = body.metadata {
        if let (Some(meta_obj), Some(rec_obj)) = (meta.as_object(), record.as_object_mut()) {
            for (k, v) in meta_obj {
                rec_obj.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }

    match milvus_upsert(&body.collection, record).await {
        Ok(_) => (axum::http::StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e}))).into_response(),
    }
}
