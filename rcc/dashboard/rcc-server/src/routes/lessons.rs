use crate::AppState;
/// /api/lessons — File-backed lesson store (Rust port of rcc/lessons/index.mjs routes)
///
/// Lessons are stored as JSONL in LESSONS_PATH (default ./data/lessons.jsonl).
/// Vector search is delegated to SOA-007 (memory/vector module).
/// For now, this implements file-based keyword search only.
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

static LESSONS_STORE: std::sync::OnceLock<RwLock<Vec<Value>>> = std::sync::OnceLock::new();
static LESSONS_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn lessons_path() -> &'static str {
    LESSONS_PATH.get_or_init(|| {
        std::env::var("LESSONS_PATH").unwrap_or_else(|_| "./data/lessons.jsonl".to_string())
    })
}

fn lessons_store() -> &'static RwLock<Vec<Value>> {
    LESSONS_STORE.get_or_init(|| RwLock::new(Vec::new()))
}

pub async fn load_lessons() {
    let path = lessons_path();
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let lessons: Vec<Value> = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let count = lessons.len();
            *lessons_store().write().await = lessons;
            tracing::info!("lessons: loaded {} from {}", count, path);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("lessons: failed to load: {}", e),
    }
}

async fn save_lesson(lesson: &Value) {
    let path = lessons_path();
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(mut line) = serde_json::to_string(lesson) {
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
    Router::new()
        .route("/api/lessons", get(get_lessons).post(post_lesson))
        .route("/api/lessons/trending", get(get_trending))
        .route("/api/lessons/heartbeat", get(get_heartbeat))
}

// ── POST /api/lessons ─────────────────────────────────────────────────────

async fn post_lesson(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    for required in &["domain", "symptom", "fix"] {
        if body
            .get(required)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .is_empty()
        {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("{} required", required)})),
            )
                .into_response();
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    let lesson = json!({
        "id":        format!("lesson-{}", chrono::Utc::now().timestamp_millis()),
        "domain":    body["domain"],
        "symptom":   body["symptom"],
        "fix":       body["fix"],
        "agent":     body.get("agent").and_then(|v| v.as_str()).unwrap_or("api"),
        "confidence": body.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.8),
        "tags":      body.get("tags").cloned().unwrap_or(json!([])),
        "createdAt": now.clone(),
        "updatedAt": now,
        "useCount":  0,
    });

    lessons_store().write().await.push(lesson.clone());
    save_lesson(&lesson).await;

    (
        axum::http::StatusCode::CREATED,
        Json(json!({"ok": true, "lesson": lesson})),
    )
        .into_response()
}

// ── GET /api/lessons ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LessonsQuery {
    domain: Option<String>,
    q: Option<String>,
    limit: Option<usize>,
    format: Option<String>,
}

async fn get_lessons(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<LessonsQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let limit = params.limit.unwrap_or(20).min(100);
    let keywords: Vec<String> = params
        .q
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let lessons = lessons_store().read().await;
    let mut results: Vec<&Value> = lessons
        .iter()
        .filter(|l| {
            // domain filter
            if let Some(d) = &params.domain {
                if l.get("domain").and_then(|v| v.as_str()) != Some(d.as_str()) {
                    return false;
                }
            }
            // keyword filter (any keyword in symptom, fix, domain, tags)
            if !keywords.is_empty() {
                let haystack = format!(
                    "{} {} {} {}",
                    l.get("symptom").and_then(|v| v.as_str()).unwrap_or(""),
                    l.get("fix").and_then(|v| v.as_str()).unwrap_or(""),
                    l.get("domain").and_then(|v| v.as_str()).unwrap_or(""),
                    l.get("tags")
                        .and_then(|v| v.as_array())
                        .map(|a| a
                            .iter()
                            .filter_map(|t| t.as_str())
                            .collect::<Vec<_>>()
                            .join(" "))
                        .unwrap_or_default(),
                )
                .to_lowercase();
                if !keywords.iter().any(|kw| haystack.contains(kw.as_str())) {
                    return false;
                }
            }
            true
        })
        .collect();

    // Sort by recency
    results.sort_by(|a, b| {
        let ta = a.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
        let tb = b.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
        tb.cmp(ta)
    });
    results.truncate(limit);
    let results: Vec<Value> = results.into_iter().cloned().collect();

    let context = if params.format.as_deref() == Some("context") {
        Some(format_for_context(&results))
    } else {
        None
    };

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "lessons": results,
            "context": context,
            "count": results.len(),
        })),
    )
        .into_response()
}

// ── GET /api/lessons/trending ─────────────────────────────────────────────

#[derive(Deserialize)]
struct TrendingQuery {
    limit: Option<usize>,
    days: Option<i64>,
    format: Option<String>,
}

async fn get_trending(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<TrendingQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let limit = params.limit.unwrap_or(5);
    let days = params.days.unwrap_or(7);
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);

    let lessons = lessons_store().read().await;

    // Count by domain in the recent window
    let mut domain_counts: HashMap<String, usize> = HashMap::new();
    for l in lessons.iter() {
        let created = l
            .get("createdAt")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        if created.map(|dt| dt >= cutoff).unwrap_or(false) {
            let domain = l
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            *domain_counts.entry(domain.to_string()).or_default() += 1;
        }
    }

    let mut sorted: Vec<_> = domain_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted.truncate(limit);

    let trending: Vec<Value> = sorted
        .into_iter()
        .map(|(domain, count)| {
            // Get most recent lesson for this domain
            let sample = lessons
                .iter()
                .filter(|l| l.get("domain").and_then(|v| v.as_str()) == Some(&domain))
                .last()
                .cloned();
            json!({ "domain": domain, "count": count, "sample": sample })
        })
        .collect();

    let context = if params.format.as_deref() == Some("context") {
        Some(format_trending_for_heartbeat(&trending))
    } else {
        None
    };

    (
        axum::http::StatusCode::OK,
        Json(json!({
            "lessons": trending,
            "context": context,
            "count": trending.len(),
        })),
    )
        .into_response()
}

// ── GET /api/lessons/heartbeat ────────────────────────────────────────────

#[derive(Deserialize)]
struct HeartbeatQuery {
    domains: Option<String>,
}

async fn get_heartbeat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<HeartbeatQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let domains: Vec<String> = params
        .domains
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let lessons = lessons_store().read().await;
    let recent: Vec<&Value> = lessons
        .iter()
        .filter(|l| {
            if domains.is_empty() {
                return true;
            }
            let d = l.get("domain").and_then(|v| v.as_str()).unwrap_or("");
            domains.iter().any(|fd| fd == d)
        })
        .rev()
        .take(5)
        .collect();

    let context = format_for_context(&recent.iter().map(|v| (*v).clone()).collect::<Vec<_>>());
    (
        axum::http::StatusCode::OK,
        Json(json!({"context": context})),
    )
        .into_response()
}

// ── Formatting helpers ─────────────────────────────────────────────────────

fn format_for_context(lessons: &[Value]) -> String {
    lessons
        .iter()
        .map(|l| {
            format!(
                "[{}] {}: {} → {}",
                l.get("domain").and_then(|v| v.as_str()).unwrap_or("?"),
                l.get("agent").and_then(|v| v.as_str()).unwrap_or("?"),
                l.get("symptom").and_then(|v| v.as_str()).unwrap_or("?"),
                l.get("fix").and_then(|v| v.as_str()).unwrap_or("?"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_trending_for_heartbeat(trending: &[Value]) -> String {
    trending
        .iter()
        .map(|t| {
            format!(
                "{} ({}x)",
                t.get("domain").and_then(|v| v.as_str()).unwrap_or("?"),
                t.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}
