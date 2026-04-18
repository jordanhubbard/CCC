use crate::AppState;
/// /api/memory/* and /api/vector/* — Qdrant vector memory (SOA-007)
///
/// Embeddings via tokenhub at http://127.0.0.1:8090/v1/embeddings (text-embedding-3-large)
/// Qdrant REST API at QDRANT_URL (default http://127.0.0.1:6333)
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const ACC_MEMORY_COLLECTION: &str = "acc_memory";
const DEFAULT_EMBED_MODEL: &str = "azure/openai/text-embedding-3-large";
const DEFAULT_EMBED_URL: &str = "https://inference-api.nvidia.com/v1";

static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
static QDRANT_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static QDRANT_API_KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
static EMBED_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static EMBED_MODEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static EMBED_KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build memory HTTP client")
    })
}

fn qdrant_base() -> &'static str {
    QDRANT_BASE.get_or_init(|| {
        std::env::var("QDRANT_URL")
            .or_else(|_| std::env::var("QDRANT_FLEET_URL"))
            .unwrap_or_else(|_| "http://127.0.0.1:6333".to_string())
    })
}

fn qdrant_api_key() -> &'static Option<String> {
    QDRANT_API_KEY.get_or_init(|| {
        std::env::var("QDRANT_API_KEY")
            .or_else(|_| std::env::var("QDRANT_FLEET_KEY"))
            .ok()
    })
}

fn embed_base() -> &'static str {
    EMBED_BASE.get_or_init(|| {
        std::env::var("NVIDIA_EMBED_URL")
            .or_else(|_| std::env::var("EMBED_URL"))
            .unwrap_or_else(|_| DEFAULT_EMBED_URL.to_string())
    })
}

fn embed_model() -> &'static str {
    EMBED_MODEL.get_or_init(|| {
        std::env::var("NVIDIA_EMBED_MODEL")
            .or_else(|_| std::env::var("EMBED_MODEL"))
            .unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string())
    })
}

fn embed_key() -> &'static Option<String> {
    EMBED_KEY.get_or_init(|| {
        std::env::var("NVIDIA_EMBED_KEY")
            .or_else(|_| std::env::var("EMBED_API_KEY"))
            .ok()
    })
}

// ── Embedding helper ──────────────────────────────────────────────────────────

async fn embed(text: &str) -> Result<Vec<f32>, String> {
    let url = format!("{}/embeddings", embed_base());
    let mut req = http_client()
        .post(&url)
        .json(&json!({ "model": embed_model(), "input": [text] }));

    if let Some(key) = embed_key() {
        req = req.bearer_auth(key);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("embed request failed: {}", e))?;

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("embed response parse failed: {}", e))?;

    body["data"][0]["embedding"]
        .as_array()
        .ok_or_else(|| format!("no embedding in response: {:?}", body))?
        .iter()
        .map(|v| {
            v.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| "non-float in embedding".to_string())
        })
        .collect()
}

// ── Content-addressed ID ──────────────────────────────────────────────────────

fn content_id(agent: &str, text: &str) -> (String, u64) {
    let prefix: String = text.chars().take(128).collect();
    let input = format!("{}:{}", agent, prefix);
    let digest = Sha256::digest(input.as_bytes());
    let id_hex = hex::encode(&digest);
    let id_u64 = u64::from_be_bytes(digest[0..8].try_into().expect("slice len 8"));
    (id_hex, id_u64)
}

// ── Qdrant helpers ────────────────────────────────────────────────────────────

fn qdrant_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    if let Some(key) = qdrant_api_key() {
        if let Ok(val) = reqwest::header::HeaderValue::from_str(key) {
            headers.insert("api-key", val);
        }
    }
    headers
}

async fn qdrant_upsert_batch(collection: &str, points: Vec<Value>) -> Result<(), String> {
    let url = format!(
        "{}/collections/{}/points?wait=true",
        qdrant_base(),
        collection
    );
    let resp = http_client()
        .put(&url)
        .headers(qdrant_headers())
        .json(&json!({ "points": points }))
        .send()
        .await
        .map_err(|e| format!("qdrant upsert request failed: {}", e))?;

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("qdrant upsert response parse failed: {}", e))?;

    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "ok" {
        return Err(format!("qdrant upsert error: {:?}", body));
    }
    Ok(())
}

async fn qdrant_upsert(collection: &str, point: Value) -> Result<(), String> {
    qdrant_upsert_batch(collection, vec![point]).await
}

async fn qdrant_search(
    collection: &str,
    vector: Vec<f32>,
    limit: usize,
    filter: Option<Value>,
) -> Result<Vec<Value>, String> {
    let mut req = json!({
        "vector": vector,
        "limit": limit,
        "with_payload": true,
    });
    if let Some(f) = filter {
        req["filter"] = f;
    }

    let url = format!("{}/collections/{}/points/search", qdrant_base(), collection);
    let resp = http_client()
        .post(&url)
        .headers(qdrant_headers())
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("qdrant search request failed: {}", e))?;

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("qdrant search response parse failed: {}", e))?;

    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "ok" {
        return Err(format!("qdrant search error: {:?}", body));
    }

    Ok(body["result"].as_array().cloned().unwrap_or_default())
}

async fn qdrant_scroll(
    collection: &str,
    filter: Option<Value>,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let mut req = json!({
        "limit": limit,
        "with_payload": true,
    });
    if let Some(f) = filter {
        req["filter"] = f;
    }

    let url = format!("{}/collections/{}/points/scroll", qdrant_base(), collection);
    let resp = http_client()
        .post(&url)
        .headers(qdrant_headers())
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("qdrant scroll request failed: {}", e))?;

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("qdrant scroll response parse failed: {}", e))?;

    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "ok" {
        return Err(format!("qdrant scroll error: {:?}", body));
    }

    Ok(body["result"]["points"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

async fn qdrant_list_collections() -> Result<Vec<String>, String> {
    let url = format!("{}/collections", qdrant_base());
    let resp = http_client()
        .get(&url)
        .headers(qdrant_headers())
        .send()
        .await
        .map_err(|e| format!("qdrant list request failed: {}", e))?;

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("qdrant list response parse failed: {}", e))?;

    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "ok" {
        return Err(format!("qdrant list error: {:?}", body));
    }

    let names = body["result"]["collections"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|c| {
            c.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    Ok(names)
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/memory/ingest", post(memory_ingest))
        .route("/api/memory/ingest/bulk", post(memory_ingest_bulk))
        .route("/api/memory/recall", get(memory_recall))
        .route("/api/memory/recent", get(memory_recent))
        .route("/api/memory/context", post(memory_context))
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
    #[serde(default)]
    source_type: Option<String>,
}

async fn memory_ingest(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<IngestBody>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    if body.text.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "text required"})),
        )
            .into_response();
    }

    let agent = body.agent.as_deref().unwrap_or("unknown");
    let (id_hex, id_u64) = content_id(agent, &body.text);
    let source = body.source.as_deref().unwrap_or("api");
    let source_type = body
        .source_type
        .as_deref()
        .unwrap_or("api")
        .chars()
        .take(64)
        .collect::<String>();
    let ts = chrono::Utc::now().timestamp_millis();

    // Log optional scope fields for tracing; not stored in ccc_memory schema
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
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    let text_trunc: String = body.text.chars().take(4096).collect();
    let agent_trunc: String = agent.chars().take(32).collect();
    let source_trunc: String = source.chars().take(64).collect();

    let point = json!({
        "id": id_u64,
        "vector": vector,
        "payload": {
            "text":        text_trunc,
            "agent":       agent_trunc,
            "source":      source_trunc,
            "source_type": source_type,
            "ingested_at": ts,
            "ts":          ts,
            "id_hex":      id_hex,
        }
    });

    match qdrant_upsert(ACC_MEMORY_COLLECTION, point).await {
        Ok(_) => (
            axum::http::StatusCode::OK,
            Json(json!({"ok": true, "id": id_hex})),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
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
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    if params.q.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "Missing query parameter q"})),
        )
            .into_response();
    }

    let vector = match embed(&params.q).await {
        Ok(v) => v,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    let k = params.k.unwrap_or(8);

    // Build Qdrant filter (agent only; platform/channel_id not in payload schema)
    tracing::debug!(platform = ?params.platform, channel_id = ?params.channel_id, "memory recall scope");
    let filter = params
        .agent
        .as_deref()
        .filter(|a| !a.is_empty())
        .map(|agent| json!({ "must": [{ "key": "agent", "match": { "value": agent } }] }));

    let raw = match qdrant_search(ACC_MEMORY_COLLECTION, vector, k, filter).await {
        Ok(r) => r,
        Err(e) if e.contains("doesn't exist") => vec![],
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    let results: Vec<Value> = raw
        .into_iter()
        .map(|r| {
            let p = &r["payload"];
            json!({
                "id":      p.get("id_hex").cloned().unwrap_or(Value::Null),
                "text":    p.get("text").cloned().unwrap_or(Value::Null),
                "agent":   p.get("agent").cloned().unwrap_or(Value::Null),
                "source":  p.get("source").cloned().unwrap_or(Value::Null),
                "score":   r.get("score").cloned().unwrap_or(Value::Null),
                "ts":      p.get("ts").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    (
        axum::http::StatusCode::OK,
        Json(json!({"ok": true, "results": results})),
    )
        .into_response()
}

// ── GET /api/vector/health ────────────────────────────────────────────────────

async fn vector_health() -> impl IntoResponse {
    match qdrant_list_collections().await {
        Ok(collections) => (
            axum::http::StatusCode::OK,
            Json(json!({"ok": true, "collections": collections})),
        )
            .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
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
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    if params.q.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "Missing query parameter q"})),
        )
            .into_response();
    }

    let vector = match embed(&params.q).await {
        Ok(v) => v,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    let k = params.k.unwrap_or(10);
    let collections_param = params.collections.as_deref().unwrap_or("all");

    let target_collections: Vec<String> = if collections_param == "all" {
        match qdrant_list_collections().await {
            Ok(cols) => cols,
            Err(e) => {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": e})),
                )
                    .into_response()
            }
        }
    } else {
        collections_param
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let mut all_results: Vec<Value> = Vec::new();
    for col in &target_collections {
        match qdrant_search(col, vector.clone(), k, None).await {
            Ok(hits) => {
                for hit in hits {
                    let p = &hit["payload"];
                    all_results.push(json!({
                        "collection": col,
                        "id":     p.get("id_hex").cloned().unwrap_or(Value::Null),
                        "text":   p.get("text").cloned().unwrap_or(Value::Null),
                        "agent":  p.get("agent").cloned().unwrap_or(Value::Null),
                        "source": p.get("source").cloned().unwrap_or(Value::Null),
                        "score":  hit.get("score").cloned().unwrap_or(Value::Null),
                        "ts":     p.get("ts").cloned().unwrap_or(Value::Null),
                    }));
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

    (
        axum::http::StatusCode::OK,
        Json(json!({"ok": true, "results": all_results})),
    )
        .into_response()
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
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    if body.collection.is_empty() || body.id.is_empty() || body.text.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "Missing required fields: collection, id, text"})),
        )
            .into_response();
    }

    let vector = match embed(&body.text).await {
        Ok(v) => v,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    // Derive numeric point ID from the provided string id via sha256
    let digest = Sha256::digest(body.id.as_bytes());
    let id_u64 = u64::from_be_bytes(digest[0..8].try_into().expect("slice len 8"));

    let mut payload = json!({
        "text":    body.text,
        "id_hex":  body.id,
    });
    if let Some(meta) = body.metadata {
        if let (Some(meta_obj), Some(pay_obj)) = (meta.as_object(), payload.as_object_mut()) {
            for (k, v) in meta_obj {
                pay_obj.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }

    let point = json!({
        "id":      id_u64,
        "vector":  vector,
        "payload": payload,
    });

    match qdrant_upsert(&body.collection, point).await {
        Ok(_) => (axum::http::StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
    }
}

// ── POST /api/memory/ingest/bulk ──────────────────────────────────────────────

#[derive(Deserialize)]
struct BulkIngestItem {
    #[serde(default)]
    id: Option<String>,
    text: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    source_type: Option<String>,
}

async fn memory_ingest_bulk(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(items): Json<Vec<BulkIngestItem>>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    if items.is_empty() {
        return (StatusCode::OK, Json(json!({"ok": true, "ingested": 0}))).into_response();
    }

    let ts = chrono::Utc::now().timestamp_millis();
    let mut points: Vec<Value> = Vec::new();

    for item in &items {
        if item.text.is_empty() {
            continue;
        }
        let agent = item.agent.as_deref().unwrap_or("unknown");
        let source = item.source.as_deref().unwrap_or("api");
        let source_type = item
            .source_type
            .as_deref()
            .unwrap_or("api")
            .chars()
            .take(64)
            .collect::<String>();

        let (id_hex, id_u64) = if let Some(s) = item.id.as_deref() {
            let digest = Sha256::digest(s.as_bytes());
            let u = u64::from_be_bytes(digest[0..8].try_into().expect("slice len 8"));
            (s.chars().take(128).collect::<String>(), u)
        } else {
            content_id(agent, &item.text)
        };

        let vector = match embed(&item.text).await {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "error": e})),
                )
                    .into_response()
            }
        };

        points.push(json!({
            "id": id_u64,
            "vector": vector,
            "payload": {
                "text":        item.text.chars().take(4096).collect::<String>(),
                "agent":       agent.chars().take(32).collect::<String>(),
                "source":      source.chars().take(64).collect::<String>(),
                "source_type": source_type,
                "ingested_at": ts,
                "ts":          ts,
                "id_hex":      id_hex,
            }
        }));
    }

    let n = points.len();
    if n == 0 {
        return (StatusCode::OK, Json(json!({"ok": true, "ingested": 0}))).into_response();
    }

    match qdrant_upsert_batch(ACC_MEMORY_COLLECTION, points).await {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "ingested": n}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": e})),
        )
            .into_response(),
    }
}

// ── GET /api/memory/recent ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RecentQuery {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    since: Option<String>,
}

async fn memory_recent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<RecentQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    let limit = params.limit.unwrap_or(20).min(100);

    // Build Qdrant filter: agent match and/or ts >= since (stored as numeric millis)
    let mut must: Vec<Value> = Vec::new();
    if let Some(agent) = &params.agent {
        if !agent.is_empty() {
            must.push(json!({ "key": "agent", "match": { "value": agent } }));
        }
    }
    if let Some(since) = &params.since {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(since) {
            let since_ms = dt.timestamp_millis();
            must.push(json!({ "key": "ts", "range": { "gte": since_ms } }));
        }
    }
    let filter = if must.is_empty() {
        None
    } else {
        Some(json!({ "must": must }))
    };

    let raw = match qdrant_scroll(ACC_MEMORY_COLLECTION, filter, limit).await {
        Ok(r) => r,
        Err(e) if e.contains("doesn't exist") => vec![],
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    let mut items: Vec<Value> = raw
        .into_iter()
        .map(|r| {
            let p = &r["payload"];
            json!({
                "id":     p.get("id_hex").cloned().unwrap_or(Value::Null),
                "text":   p.get("text").cloned().unwrap_or(Value::Null),
                "agent":  p.get("agent").cloned().unwrap_or(Value::Null),
                "source": p.get("source").cloned().unwrap_or(Value::Null),
                "ts":     p.get("ts").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    items.sort_by(|a, b| {
        let ta = a.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
        let tb = b.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
        tb.cmp(&ta)
    });

    (StatusCode::OK, Json(json!({"ok": true, "items": items}))).into_response()
}

// ── POST /api/memory/context ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct ContextBody {
    query: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    k: Option<usize>,
    #[serde(default)]
    max_tokens: Option<usize>,
}

async fn memory_context(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ContextBody>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    if body.query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "query required"})),
        )
            .into_response();
    }

    let k = body.k.unwrap_or(8);
    let max_chars = body.max_tokens.unwrap_or(1500) * 4;

    let vector = match embed(&body.query).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    let filter = body
        .agent
        .as_deref()
        .filter(|a| !a.is_empty())
        .map(|agent| json!({ "must": [{ "key": "agent", "match": { "value": agent } }] }));

    let raw = match qdrant_search(ACC_MEMORY_COLLECTION, vector, k, filter).await {
        Ok(r) => r,
        Err(e) if e.contains("doesn't exist") => vec![],
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "error": e})),
            )
                .into_response()
        }
    };

    let items: Vec<Value> = raw
        .into_iter()
        .map(|r| {
            let p = &r["payload"];
            json!({
                "id":     p.get("id_hex").cloned().unwrap_or(Value::Null),
                "text":   p.get("text").cloned().unwrap_or(Value::Null),
                "agent":  p.get("agent").cloned().unwrap_or(Value::Null),
                "source": p.get("source").cloned().unwrap_or(Value::Null),
                "ts":     p.get("ts").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    let mut context = String::from("FLEET MEMORY CONTEXT:\n");
    let mut truncated = false;

    for item in &items {
        let agent_str = item
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let source_str = item.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let content_str = item.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let line = format!("[{}] {}: {}\n", agent_str, source_str, content_str);

        if context.len() + line.len() > max_chars {
            truncated = true;
            break;
        }
        context.push_str(&line);
    }

    (
        StatusCode::OK,
        Json(json!({"ok": true, "context": context, "items": items, "truncated": truncated})),
    )
        .into_response()
}
