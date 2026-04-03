use crate::routes::metrics;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::{get, patch},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use crate::AppState;

/// In-memory GitHub cache entry
struct GhCacheEntry {
    data: Value,
    fetched_at: Instant,
}

/// Shared GitHub cache (full_name → entry)
static GH_CACHE: std::sync::OnceLock<RwLock<HashMap<String, GhCacheEntry>>> =
    std::sync::OnceLock::new();

fn gh_cache() -> &'static RwLock<HashMap<String, GhCacheEntry>> {
    GH_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/:owner/:repo/github", get(project_github))
        .route("/api/projects/:owner/:repo", get(get_project))
        .route("/api/projects/:id", patch(update_project).delete(delete_project))
        .merge(metrics::router())
}

// ── helpers ──────────────────────────────────────────────────────────────

async fn read_projects(state: &AppState) -> Vec<Value> {
    let lock = state.projects.read().await;
    lock.clone()
}

async fn write_projects(state: &AppState, projects: Vec<Value>) {
    {
        let mut lock = state.projects.write().await;
        *lock = projects.clone();
    }
    // Persist to disk
    if let Ok(json) = serde_json::to_string_pretty(&projects) {
        let _ = tokio::fs::write(&state.projects_path, json).await;
    }
}

// ── GET /api/projects ─────────────────────────────────────────────────────

async fn list_projects(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let projects = read_projects(&state).await;
    Json(projects)
}

// ── GET /api/projects/:owner/:repo ────────────────────────────────────────

async fn get_project(
    State(state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
) -> impl IntoResponse {
    let full_name = format!("{}/{}", owner, repo);
    let projects = read_projects(&state).await;
    match projects.into_iter().find(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(&full_name)
            || p.get("full_name").and_then(|v| v.as_str()) == Some(&full_name)
    }) {
        Some(p) => (axum::http::StatusCode::OK, Json(p)).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "Project not found"})),
        ).into_response(),
    }
}

// ── GET /api/projects/:owner/:repo/github ────────────────────────────────

async fn project_github(
    State(_state): State<Arc<AppState>>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let full_name = format!("{}/{}", owner, repo);
    let bust_cache = params.get("refresh").map(|v| v == "1").unwrap_or(false);

    // Check cache (5 min TTL)
    if !bust_cache {
        let cache = gh_cache().read().await;
        if let Some(entry) = cache.get(&full_name) {
            if entry.fetched_at.elapsed() < Duration::from_secs(300) {
                return (axum::http::StatusCode::OK, Json(entry.data.clone())).into_response();
            }
        }
    }

    // Run `gh issue list`
    let issues = run_gh_json(
        &format!(
            "issue list --repo {} --state open --limit 50 --json number,title,labels,url,author,createdAt,updatedAt,comments",
            full_name
        )
    ).unwrap_or_else(|_| json!([]));

    // Run `gh pr list`
    let prs = run_gh_json(
        &format!(
            "pr list --repo {} --state open --limit 30 --json number,title,author,url,isDraft,reviewDecision,mergeable,createdAt,updatedAt,labels",
            full_name
        )
    ).unwrap_or_else(|_| json!([]));

    let result = json!({
        "repo": full_name,
        "fetchedAt": chrono::Utc::now().to_rfc3339(),
        "issues": normalize_issues(&issues),
        "prs": normalize_prs(&prs),
    });

    // Update cache
    {
        let mut cache = gh_cache().write().await;
        cache.insert(full_name, GhCacheEntry {
            data: result.clone(),
            fetched_at: Instant::now(),
        });
    }

    (axum::http::StatusCode::OK, Json(result)).into_response()
}

fn run_gh_json(args: &str) -> Result<Value, String> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let output = std::process::Command::new("gh")
        .args(&parts)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())
}

fn normalize_issues(issues: &Value) -> Value {
    let arr = issues.as_array().cloned().unwrap_or_default();
    json!(arr.iter().map(|i| json!({
        "number": i["number"],
        "title":  i["title"],
        "url":    i["url"],
        "labels": i["labels"].as_array().unwrap_or(&vec![]).iter().map(|l| json!({
            "name": l["name"], "color": l["color"]
        })).collect::<Vec<_>>(),
        "author":       i["author"]["login"].as_str().unwrap_or_else(|| i["author"].as_str().unwrap_or("")),
        "createdAt":    i["createdAt"],
        "updatedAt":    i["updatedAt"],
        "commentCount": i["comments"].as_array().map(|a| a.len()).unwrap_or(0),
    })).collect::<Vec<_>>())
}

fn normalize_prs(prs: &Value) -> Value {
    let arr = prs.as_array().cloned().unwrap_or_default();
    json!(arr.iter().map(|p| json!({
        "number":         p["number"],
        "title":          p["title"],
        "url":            p["url"],
        "author":         p["author"]["login"].as_str().unwrap_or_else(|| p["author"].as_str().unwrap_or("")),
        "isDraft":        p["isDraft"].as_bool().unwrap_or(false),
        "reviewDecision": p["reviewDecision"],
        "mergeable":      p["mergeable"],
        "createdAt":      p["createdAt"],
        "updatedAt":      p["updatedAt"],
        "labels": p["labels"].as_array().unwrap_or(&vec![]).iter().map(|l| json!({
            "name": l["name"], "color": l["color"]
        })).collect::<Vec<_>>(),
    })).collect::<Vec<_>>())
}

// ── POST /api/projects ────────────────────────────────────────────────────

async fn create_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error":"name required"}))).into_response(),
    };
    let id = format!("proj-{}", chrono::Utc::now().timestamp_millis());
    let now = chrono::Utc::now().to_rfc3339();
    let project = json!({
        "id":           id,
        "name":         name,
        "description":  body.get("description").cloned().unwrap_or(json!("")),
        "repoUrl":      body.get("repoUrl").cloned().unwrap_or(json!(null)),
        "slackChannels": body.get("slackChannels").cloned().unwrap_or(json!([])),
        "tags":         body.get("tags").cloned().unwrap_or(json!([])),
        "status":       body.get("status").and_then(|v| v.as_str()).unwrap_or("active"),
        "createdAt":    now.clone(),
        "updatedAt":    now,
    });
    let mut projects = read_projects(&state).await;
    projects.push(project.clone());
    write_projects(&state, projects).await;
    (axum::http::StatusCode::CREATED, Json(json!({"ok": true, "project": project}))).into_response()
}

// ── PATCH /api/projects/:id ───────────────────────────────────────────────

async fn update_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let mut projects = read_projects(&state).await;
    let idx = projects.iter().position(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(&id)
    });
    match idx {
        None => (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"Project not found"}))).into_response(),
        Some(i) => {
            let p = projects[i].as_object_mut().unwrap();
            for field in &["name", "description", "repoUrl", "slackChannels", "tags", "status"] {
                if let Some(v) = body.get(field) {
                    p.insert(field.to_string(), v.clone());
                }
            }
            p.insert("updatedAt".to_string(), json!(chrono::Utc::now().to_rfc3339()));
            let updated = projects[i].clone();
            write_projects(&state, projects).await;
            (axum::http::StatusCode::OK, Json(json!({"ok": true, "project": updated}))).into_response()
        }
    }
}

// ── DELETE /api/projects/:id ──────────────────────────────────────────────

async fn delete_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let mut projects = read_projects(&state).await;
    let idx = projects.iter().position(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(&id)
    });
    match idx {
        None => (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"Project not found"}))).into_response(),
        Some(i) => {
            let p = projects[i].as_object_mut().unwrap();
            p.insert("status".to_string(), json!("archived"));
            p.insert("updatedAt".to_string(), json!(chrono::Utc::now().to_rfc3339()));
            let archived = projects[i].clone();
            write_projects(&state, projects).await;
            (axum::http::StatusCode::OK, Json(json!({"ok": true, "project": archived}))).into_response()
        }
    }
}
