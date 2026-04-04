use crate::AppState;
/// /api/projects/:owner/:repo/metrics — Project CI metrics store
///
/// Persists metrics (e.g. coverage_pct, test_count, build_time) as JSONL.
/// Agents and CI pipelines POST new data points; the dashboard reads a sparkline.
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

/// A single time-series data point stored in the metrics table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    pub ts: String,
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

static METRICS_STORE: std::sync::OnceLock<RwLock<Vec<Value>>> = std::sync::OnceLock::new();
static METRICS_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn metrics_path() -> &'static str {
    METRICS_PATH.get_or_init(|| {
        std::env::var("METRICS_PATH").unwrap_or_else(|_| "./data/metrics.jsonl".to_string())
    })
}

fn metrics_store() -> &'static RwLock<Vec<Value>> {
    METRICS_STORE.get_or_init(|| RwLock::new(Vec::new()))
}

pub async fn load_metrics() {
    let path = metrics_path();
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let entries: Vec<Value> = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let count = entries.len();
            *metrics_store().write().await = entries;
            tracing::info!("metrics: loaded {} from {}", count, path);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("metrics: failed to load: {}", e),
    }
}

async fn append_metric(entry: &Value) {
    let path = metrics_path();
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(mut line) = serde_json::to_string(entry) {
        line.push('\n');
        use tokio::io::AsyncWriteExt;
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            let _ = f.write_all(line.as_bytes()).await;
        }
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/projects/:owner/:repo/metrics",
        get(get_metrics).post(post_metric),
    )
}

// ── POST /api/projects/:owner/:repo/metrics ───────────────────────────────

async fn post_metric(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    let metric = match body.get("metric").and_then(|v| v.as_str()) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": "metric required"})),
            )
                .into_response();
        }
    };

    let value = match body.get("value").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": "value (number) required"})),
            )
                .into_response();
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    let repo_full = format!("{}/{}", owner, repo);
    let entry = json!({
        "id":         format!("metric-{}", chrono::Utc::now().timestamp_millis()),
        "repo":       repo_full,
        "metric":     metric,
        "value":      value,
        "commit_sha": body.get("commit_sha").and_then(|v| v.as_str()).unwrap_or(""),
        "branch":     body.get("branch").and_then(|v| v.as_str()).unwrap_or("main"),
        "ts":         body.get("ts").and_then(|v| v.as_str()).unwrap_or(&now).to_string(),
        "createdAt":  now,
    });

    metrics_store().write().await.push(entry.clone());
    append_metric(&entry).await;

    (
        axum::http::StatusCode::CREATED,
        Json(json!({"ok": true, "entry": entry})),
    )
        .into_response()
}

// ── GET /api/projects/:owner/:repo/metrics ─────────────────────────────────

#[derive(Deserialize)]
struct MetricsQuery {
    metric: Option<String>,
    limit: Option<usize>,
    branch: Option<String>,
}

async fn get_metrics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<MetricsQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    let repo_full = format!("{}/{}", owner, repo);
    let limit = params.limit.unwrap_or(50).min(500);

    let store = metrics_store().read().await;
    let mut results: Vec<&Value> = store
        .iter()
        .filter(|e| {
            e.get("repo").and_then(|v| v.as_str()) == Some(&repo_full)
                && params
                    .metric
                    .as_deref()
                    .map(|m| e.get("metric").and_then(|v| v.as_str()) == Some(m))
                    .unwrap_or(true)
                && params
                    .branch
                    .as_deref()
                    .map(|b| e.get("branch").and_then(|v| v.as_str()) == Some(b))
                    .unwrap_or(true)
        })
        .collect();

    // Most recent first
    results.sort_by(|a, b| {
        let ta = a.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });
    results.truncate(limit);
    let results: Vec<Value> = results.into_iter().cloned().collect();

    // Build sparkline: group by metric name, latest N values
    let mut sparklines: std::collections::HashMap<String, Vec<f64>> =
        std::collections::HashMap::new();
    for e in &results {
        let m = e.get("metric").and_then(|v| v.as_str()).unwrap_or("?");
        let v = e.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
        sparklines.entry(m.to_string()).or_default().push(v);
    }

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "repo": repo_full,
            "count": results.len(),
            "entries": results,
            "sparklines": sparklines,
        })),
    )
        .into_response()
}
