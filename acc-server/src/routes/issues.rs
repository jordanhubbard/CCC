use crate::AppState;
/// /api/issues — File-backed GitHub issues store (Rust port of Node.js issues routes)
///
/// Issues stored as JSON in ISSUES_PATH (default ./data/issues.json).
/// Format: {"issues": [...], "lastSync": {"repo": "timestamp"}}
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

static ISSUES_STORE: std::sync::OnceLock<RwLock<Vec<Value>>> = std::sync::OnceLock::new();
static LAST_SYNC: std::sync::OnceLock<RwLock<HashMap<String, String>>> = std::sync::OnceLock::new();
static ISSUES_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn issues_path() -> &'static str {
    ISSUES_PATH.get_or_init(|| {
        std::env::var("ISSUES_PATH").unwrap_or_else(|_| "./data/issues.json".to_string())
    })
}

fn issues_store() -> &'static RwLock<Vec<Value>> {
    ISSUES_STORE.get_or_init(|| RwLock::new(Vec::new()))
}

fn last_sync() -> &'static RwLock<HashMap<String, String>> {
    LAST_SYNC.get_or_init(|| RwLock::new(HashMap::new()))
}

pub async fn load_issues() {
    let path = issues_path();
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            if let Ok(data) = serde_json::from_str::<Value>(&content) {
                if let Some(issues) = data.get("issues").and_then(|v| v.as_array()).cloned() {
                    let count = issues.len();
                    *issues_store().write().await = issues;
                    tracing::info!("issues: loaded {} from {}", count, path);
                }
                if let Some(sync_map) = data.get("lastSync").and_then(|v| v.as_object()) {
                    let map: HashMap<String, String> = sync_map
                        .iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect();
                    *last_sync().write().await = map;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("issues: failed to load: {}", e),
    }
}

async fn save_issues() {
    let path = issues_path();
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let issues = issues_store().read().await.clone();
    let sync: HashMap<String, String> = last_sync().read().await.clone();
    let sync_val: serde_json::Map<String, Value> =
        sync.into_iter().map(|(k, v)| (k, json!(v))).collect();
    let data = json!({"issues": issues, "lastSync": sync_val});
    if let Ok(content) = serde_json::to_string_pretty(&data) {
        let tmp = format!("{}.tmp", path);
        if tokio::fs::write(&tmp, &content).await.is_ok() {
            let _ = tokio::fs::rename(&tmp, path).await;
        }
    }
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/issues", get(list_issues))
        // static paths before :id to avoid capture
        .route("/api/issues/sync", post(sync_issues))
        .route("/api/issues/create-from-wq", post(create_from_wq))
        .route("/api/issues/:id", get(get_issue).patch(patch_issue).delete(delete_issue))
        .route("/api/issues/:id/link", post(link_issue))
}

// ── GET /api/issues ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct IssuesQuery {
    repo: Option<String>,
    state: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list_issues(
    State(_state): State<Arc<AppState>>,
    Query(params): Query<IssuesQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(200);
    let offset = params.offset.unwrap_or(0);
    let filter_state = params.state.as_deref().unwrap_or("open").to_string();

    let issues = issues_store().read().await;
    let filtered: Vec<&Value> = issues
        .iter()
        .filter(|i| {
            if let Some(repo) = &params.repo {
                if i.get("repo").and_then(|v| v.as_str()) != Some(repo.as_str()) {
                    return false;
                }
            }
            if filter_state != "all" {
                let s = i.get("state").and_then(|v| v.as_str()).unwrap_or("open");
                if s != filter_state {
                    return false;
                }
            }
            true
        })
        .collect();

    let count = filtered.len();
    let page: Vec<Value> = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    drop(issues);

    let last_sync_val: Option<String> = if let Some(repo) = &params.repo {
        last_sync().read().await.get(repo.as_str()).cloned()
    } else {
        None
    };

    Json(json!({
        "ok": true,
        "issues": page,
        "count": count,
        "lastSync": last_sync_val,
    }))
}

// ── GET /api/issues/:id ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct IssueByIdQuery {
    repo: Option<String>,
}

async fn get_issue(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<u64>,
    Query(params): Query<IssueByIdQuery>,
) -> impl IntoResponse {
    let issues = issues_store().read().await;
    let found = issues
        .iter()
        .find(|i| {
            let num_match = i.get("number").and_then(|v| v.as_u64()) == Some(id);
            if !num_match {
                return false;
            }
            if let Some(repo) = &params.repo {
                i.get("repo").and_then(|v| v.as_str()) == Some(repo.as_str())
            } else {
                true
            }
        })
        .cloned();

    match found {
        Some(issue) => (
            axum::http::StatusCode::OK,
            Json(json!({"ok": true, "issue": issue})),
        )
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "Issue not found"})),
        )
            .into_response(),
    }
}

// ── PATCH /api/issues/:id ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct PatchIssueBody {
    repo: Option<String>,
    state: Option<String>,
    title: Option<String>,
    labels: Option<Value>,
    assignee: Option<String>,
}

async fn patch_issue(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<u64>,
    Json(body): Json<PatchIssueBody>,
) -> impl IntoResponse {
    let updated = {
        let mut store = issues_store().write().await;
        match store.iter_mut().find(|i| {
            i.get("number").and_then(|v| v.as_u64()) == Some(id)
                && body.repo.as_deref().map_or(true, |r| i.get("repo").and_then(|v| v.as_str()) == Some(r))
        }) {
            None => None,
            Some(issue) => {
                let obj = issue.as_object_mut().unwrap();
                if let Some(state) = &body.state { obj.insert("state".into(), json!(state)); }
                if let Some(title) = &body.title { obj.insert("title".into(), json!(title)); }
                if let Some(labels) = &body.labels { obj.insert("labels".into(), labels.clone()); }
                if let Some(assignee) = &body.assignee { obj.insert("assignee".into(), json!(assignee)); }
                obj.insert("updatedAt".into(), json!(chrono::Utc::now().to_rfc3339()));
                Some(issue.clone())
            }
        }
    };
    match updated {
        None => (axum::http::StatusCode::NOT_FOUND, Json(json!({"error": "Issue not found"}))).into_response(),
        Some(issue) => {
            save_issues().await;
            Json(json!({"ok": true, "issue": issue})).into_response()
        }
    }
}

// ── DELETE /api/issues/:id ────────────────────────────────────────────────

#[derive(Deserialize)]
struct DeleteIssueQuery {
    repo: Option<String>,
}

async fn delete_issue(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<u64>,
    Query(params): Query<DeleteIssueQuery>,
) -> impl IntoResponse {
    let mut store = issues_store().write().await;
    let idx = store.iter().position(|i| {
        i.get("number").and_then(|v| v.as_u64()) == Some(id)
            && params.repo.as_deref().map_or(true, |r| i.get("repo").and_then(|v| v.as_str()) == Some(r))
    });
    match idx {
        None => (axum::http::StatusCode::NOT_FOUND, Json(json!({"error": "Issue not found"}))).into_response(),
        Some(i) => {
            store.remove(i);
            drop(store);
            save_issues().await;
            Json(json!({"ok": true, "number": id, "deleted": true})).into_response()
        }
    }
}

// ── POST /api/issues/sync ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct SyncBody {
    repo: Option<String>,
    state: Option<String>,
}

async fn sync_issues(
    State(_state): State<Arc<AppState>>,
    Json(body): Json<SyncBody>,
) -> impl IntoResponse {
    let state_filter = body.state.as_deref().unwrap_or("all").to_string();
    match body.repo {
        None => Json(json!({"ok": true, "result": {"synced": 0, "repo": null}})).into_response(),
        Some(repo) => match do_sync_repo(&repo, &state_filter).await {
            Ok(synced) => Json(json!({"ok": true, "result": {"synced": synced, "repo": repo}}))
                .into_response(),
            Err(e) => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e})),
            )
                .into_response(),
        },
    }
}

async fn do_sync_repo(repo: &str, state_filter: &str) -> Result<usize, String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            state_filter,
            "--limit",
            "100",
            "--json",
            "number,title,body,labels,url,author,createdAt,updatedAt,state,comments",
        ])
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    let fetched: Vec<Value> = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let count = fetched.len();
    let now = chrono::Utc::now().to_rfc3339();

    {
        let mut store = issues_store().write().await;
        for mut issue in fetched {
            if let Some(obj) = issue.as_object_mut() {
                obj.insert("repo".to_string(), json!(repo));
                // Normalize author object → login string
                if let Some(author) = obj.get("author").cloned() {
                    if let Some(login) = author.get("login").and_then(|v| v.as_str()) {
                        obj.insert("author".to_string(), json!(login));
                    }
                }
            }
            let number = issue.get("number").and_then(|v| v.as_u64());
            if let Some(num) = number {
                if let Some(pos) = store.iter().position(|e| {
                    e.get("number").and_then(|v| v.as_u64()) == Some(num)
                        && e.get("repo").and_then(|v| v.as_str()) == Some(repo)
                }) {
                    // Preserve existing wq_id link on upsert
                    let existing_wq = store[pos].get("wq_id").cloned();
                    if let (Some(obj), Some(wq)) = (issue.as_object_mut(), existing_wq) {
                        obj.insert("wq_id".to_string(), wq);
                    }
                    store[pos] = issue;
                } else {
                    store.push(issue);
                }
            }
        }
    }

    last_sync().write().await.insert(repo.to_string(), now);
    save_issues().await;
    Ok(count)
}

// ── POST /api/issues/:id/link ──────────────────────────────────────────────

#[derive(Deserialize)]
struct LinkBody {
    repo: String,
    wq_id: String,
}

async fn link_issue(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<u64>,
    Json(body): Json<LinkBody>,
) -> impl IntoResponse {
    let updated = {
        let mut store = issues_store().write().await;
        match store.iter_mut().find(|i| {
            i.get("number").and_then(|v| v.as_u64()) == Some(id)
                && i.get("repo").and_then(|v| v.as_str()) == Some(body.repo.as_str())
        }) {
            None => None,
            Some(issue) => {
                if let Some(obj) = issue.as_object_mut() {
                    obj.insert("wq_id".to_string(), json!(body.wq_id));
                }
                Some(issue.clone())
            }
        }
    };

    match updated {
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "Issue not found"})),
        )
            .into_response(),
        Some(issue) => {
            save_issues().await;
            (
                axum::http::StatusCode::OK,
                Json(json!({"ok": true, "issue": issue})),
            )
                .into_response()
        }
    }
}

// ── POST /api/issues/create-from-wq ───────────────────────────────────────

#[derive(Deserialize)]
struct CreateFromWqBody {
    wq_id: String,
    repo: String,
}

async fn create_from_wq(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateFromWqBody>,
) -> impl IntoResponse {
    let item = {
        let queue = state.queue.read().await;
        queue
            .items
            .iter()
            .chain(queue.completed.iter())
            .find(|i| i.get("id").and_then(|v| v.as_str()) == Some(body.wq_id.as_str()))
            .cloned()
    };

    let item = match item {
        Some(i) => i,
        None => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({"error": format!("WQ item {} not found", body.wq_id)})),
            )
                .into_response()
        }
    };

    let title = item
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("(no title)")
        .to_string();
    let description = item
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let output = tokio::process::Command::new("gh")
        .args([
            "issue",
            "create",
            "--repo",
            &body.repo,
            "--title",
            &title,
            "--body",
            &description,
        ])
        .output()
        .await;

    match output {
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
        Ok(out) if !out.status.success() => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": String::from_utf8_lossy(&out.stderr).trim().to_string()})),
        )
            .into_response(),
        Ok(out) => {
            // `gh issue create` prints the issue URL on the last line
            let stdout = String::from_utf8_lossy(&out.stdout);
            let url = stdout.lines().last().unwrap_or("").trim().to_string();
            let number: Option<u64> = url.split('/').last().and_then(|s| s.parse().ok());
            (
                axum::http::StatusCode::CREATED,
                Json(json!({"ok": true, "url": url, "number": number})),
            )
                .into_response()
        }
    }
}
