use crate::routes::metrics;
use rusqlite;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::{get, post},
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
        .route("/api/projects/:id", get(get_project_by_id).patch(update_project).delete(delete_project))
        .route("/api/projects/:id/import-beads", post(import_beads))
        .route("/api/projects/:id/clean", post(mark_project_clean))
        .route("/api/projects/:id/refresh", post(refresh_project_workspace))
        .route("/api/projects/:id/phase-commit-failed", post(report_phase_commit_failure))
        .merge(metrics::router())
}

/// Set agentfs_dirty on a project. Used by:
///   - POST /api/projects/:id/clean       → set false (milestone-commit task)
///   - tasks::complete_task on completion → set true (any task touched AgentFS)
///
/// Returns true if the project existed and the field was set.
pub async fn set_agentfs_dirty(state: &Arc<AppState>, project_id: &str, dirty: bool) -> bool {
    let mut projects = read_projects(state).await;
    let mut found = false;
    for p in projects.iter_mut() {
        if p.get("id").and_then(|v| v.as_str()) == Some(project_id) {
            if let Some(obj) = p.as_object_mut() {
                obj.insert("agentfs_dirty".to_string(), json!(dirty));
                obj.insert("updatedAt".to_string(), json!(chrono::Utc::now().to_rfc3339()));
            }
            found = true;
            break;
        }
    }
    if found {
        write_projects(state, projects).await;
    }
    found
}

// ── POST /api/projects/:id/clean ──────────────────────────────────────────
//
// Marks the project's AgentFS state as clean (committed and pushed to git).
// Called by the milestone-commit task after a successful push.
async fn mark_project_clean(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    if !set_agentfs_dirty(&state, &id, false).await {
        return (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"Project not found"}))).into_response();
    }
    // Successful phase_commit also resets the consecutive-failure counter.
    let _ = set_phase_commit_failures(&state, &id, 0).await;
    let _ = state.bus_tx.send(
        json!({"type":"projects:agentfs_clean","project_id":id}).to_string()
    );
    (axum::http::StatusCode::OK, Json(json!({"ok":true,"project_id":id,"agentfs_dirty":false}))).into_response()
}

// ── POST /api/projects/:id/refresh ────────────────────────────────────────
//
// Pull origin/<default> into the project's AgentFS workspace so agents
// see the latest upstream state. Refuses when agentfs_dirty=true to
// avoid clobbering uncommitted local edits.
async fn refresh_project_workspace(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let projects = read_projects(&state).await;
    let project = projects.iter().find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&id));
    let project = match project {
        Some(p) => p.clone(),
        None => return (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"Project not found"}))).into_response(),
    };
    let path = project.get("agentfs_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if path.is_empty() {
        return (axum::http::StatusCode::CONFLICT, Json(json!({"error":"project has no agentfs_path"}))).into_response();
    }
    let dirty = project.get("agentfs_dirty").and_then(|v| v.as_bool()).unwrap_or(false);
    if dirty {
        return (axum::http::StatusCode::CONFLICT, Json(json!({
            "error":"agentfs_dirty",
            "message":"refusing to refresh — uncommitted changes in AgentFS. phase_commit first."
        }))).into_response();
    }

    match git_pull_workspace(&path).await {
        Ok(summary) => {
            let _ = state.bus_tx.send(json!({
                "type":"projects:refreshed","project_id":id,"summary":summary
            }).to_string());
            (axum::http::StatusCode::OK, Json(json!({"ok":true,"project_id":id,"summary":summary}))).into_response()
        }
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":e}))).into_response(),
    }
}

/// Run `git fetch origin && git merge --ff-only origin/<branch>` in `path`.
/// Returns a short status string. Errors propagate stderr text on failure.
pub async fn git_pull_workspace(path: &str) -> Result<String, String> {
    if !std::path::Path::new(path).join(".git").exists() {
        return Err(format!("not a git workspace: {path}"));
    }
    // Determine current branch (usually main)
    let branch_out = tokio::process::Command::new("git")
        .args(["-C", path, "rev-parse", "--abbrev-ref", "HEAD"])
        .output().await
        .map_err(|e| format!("git rev-parse: {e}"))?;
    let branch = if branch_out.status.success() {
        String::from_utf8_lossy(&branch_out.stdout).trim().to_string()
    } else {
        "main".to_string()
    };

    let fetch = tokio::process::Command::new("git")
        .args(["-C", path, "fetch", "origin", "--quiet"])
        .output().await
        .map_err(|e| format!("git fetch: {e}"))?;
    if !fetch.status.success() {
        return Err(format!("fetch failed: {}", String::from_utf8_lossy(&fetch.stderr)));
    }

    let merge = tokio::process::Command::new("git")
        .args(["-C", path, "merge", "--ff-only", &format!("origin/{branch}"), "--quiet"])
        .output().await
        .map_err(|e| format!("git merge: {e}"))?;
    if merge.status.success() {
        Ok(format!("fast-forwarded {branch}"))
    } else {
        let stderr = String::from_utf8_lossy(&merge.stderr).to_string();
        if stderr.contains("Already up to date") || stderr.is_empty() {
            Ok("already up to date".to_string())
        } else {
            Err(format!("merge --ff-only failed (likely diverged): {stderr}"))
        }
    }
}

/// Set the consecutive phase_commit failure counter on a project.
/// Used by both the failure reporter (increment) and /clean (reset to 0).
pub async fn set_phase_commit_failures(state: &Arc<AppState>, project_id: &str, value: i64) -> bool {
    let mut projects = read_projects(state).await;
    let mut found = false;
    for p in projects.iter_mut() {
        if p.get("id").and_then(|v| v.as_str()) == Some(project_id) {
            if let Some(obj) = p.as_object_mut() {
                obj.insert("phase_commit_consecutive_failures".to_string(), json!(value));
                obj.insert("updatedAt".to_string(), json!(chrono::Utc::now().to_rfc3339()));
            }
            found = true;
            break;
        }
    }
    if found { write_projects(state, projects).await; }
    found
}

/// Read the current consecutive phase_commit failure counter for a project.
pub async fn get_phase_commit_failures(state: &Arc<AppState>, project_id: &str) -> i64 {
    let projects = read_projects(state).await;
    projects.iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(project_id))
        .and_then(|p| p.get("phase_commit_consecutive_failures").and_then(|v| v.as_i64()))
        .unwrap_or(0)
}

// ── POST /api/projects/:id/phase-commit-failed ────────────────────────────
//
// Agents call this when their phase_commit attempt fails (push failed,
// merge failed, etc). The dispatch loop reads the counter and stops
// auto-filing phase_commits after 3 consecutive failures so we don't
// pile up a queue of doomed attempts.
async fn report_phase_commit_failure(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let cur = get_phase_commit_failures(&state, &id).await;
    let next = cur + 1;
    if !set_phase_commit_failures(&state, &id, next).await {
        return (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"Project not found"}))).into_response();
    }
    let reason = body.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let _ = state.bus_tx.send(json!({
        "type":"projects:phase_commit_failed",
        "project_id":id,
        "consecutive_failures":next,
        "reason":reason,
    }).to_string());
    (axum::http::StatusCode::OK, Json(json!({
        "ok":true,
        "project_id":id,
        "consecutive_failures":next,
    }))).into_response()
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
//
// Query params:
//   status=<value>   — filter by exact status (e.g. "active", "archived")
//   tag=<value>      — filter to projects whose tags array contains value
//   q=<text>         — case-insensitive substring match on name, slug, description
//   limit=N          — return at most N results (default: all)
//   offset=N         — skip first N results (for pagination)

async fn list_projects(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let projects = read_projects(&state).await;

    let status_filter = params.get("status").map(|s| s.as_str());
    let tag_filter    = params.get("tag").map(|s| s.to_lowercase());
    let q             = params.get("q").map(|s| s.to_lowercase());
    let limit: Option<usize>  = params.get("limit").and_then(|s| s.parse().ok());
    let offset: usize = params.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);

    let filtered: Vec<&Value> = projects.iter().filter(|p| {
        if let Some(st) = status_filter {
            if p.get("status").and_then(|v| v.as_str()) != Some(st) { return false; }
        }
        if let Some(ref tag) = tag_filter {
            let has_tag = p.get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().any(|t| {
                    t.as_str().map(|s| s.to_lowercase() == *tag).unwrap_or(false)
                }))
                .unwrap_or(false);
            if !has_tag { return false; }
        }
        if let Some(ref q) = q {
            let name  = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            let slug  = p.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            let desc  = p.get("description").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            if !name.contains(q.as_str()) && !slug.contains(q.as_str()) && !desc.contains(q.as_str()) {
                return false;
            }
        }
        true
    }).collect();

    let total = filtered.len();
    let page: Vec<Value> = filtered.into_iter().skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .cloned()
        .collect();

    Json(json!({"projects": page, "total": total, "offset": offset}))
}

// ── GET /api/projects/:id ─────────────────────────────────────────────────

async fn get_project_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let projects = read_projects(&state).await;
    match projects.into_iter().find(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(&id)
            || p.get("slug").and_then(|v| v.as_str()) == Some(&id)
    }) {
        Some(p) => (axum::http::StatusCode::OK, Json(p)).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error": "Project not found"})),
        ).into_response(),
    }
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

    // Compute slug: lowercase, spaces → hyphens, strip non-alphanumeric except hyphens
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();

    let git_url = body.get("git_url").and_then(|v| v.as_str()).map(|s| s.to_string());
    let agentfs_path = format!("/srv/accfs/shared/{}", slug);

    let clone_status = if git_url.is_some() { "pending" } else { "none" };

    let project = json!({
        "id":           id,
        "name":         name,
        "slug":         slug.clone(),
        "agentfs_path": agentfs_path.clone(),
        "git_url":      git_url.clone().map(Value::String).unwrap_or(Value::Null),
        "clone_status": clone_status,
        "agentfs_dirty": false,
        "description":  body.get("description").cloned().unwrap_or(json!("")),
        "repoUrl":      body.get("repoUrl").cloned().unwrap_or(json!(null)),
        "slackChannels": body.get("slackChannels").cloned().unwrap_or(json!([])),
        "tags":         body.get("tags").cloned().unwrap_or(json!([])),
        "status":       body.get("status").and_then(|v| v.as_str()).unwrap_or("active"),
        "owner":        body.get("owner").cloned().unwrap_or(json!(null)),
        "assignee":     body.get("assignee").cloned().unwrap_or(json!(null)),
        "notes":        body.get("notes").cloned().unwrap_or(json!("")),
        "createdAt":    now.clone(),
        "updatedAt":    now,
    });
    let mut projects = read_projects(&state).await;
    projects.push(project.clone());
    write_projects(&state, projects).await;

    // Broadcast project registration
    let proj_id = id.clone();
    let proj_slug = slug.clone();
    let _ = state.bus_tx.send(json!({"type":"projects:registered","project_id":proj_id,"slug":proj_slug}).to_string());

    // Spawn background git-clone or directory creation
    let state_clone = state.clone();
    let agentfs_path_clone = agentfs_path.clone();
    let id_clone = id.clone();
    tokio::spawn(async move {
        if let Some(url) = git_url {
            // Run git clone
            let output = tokio::process::Command::new("git")
                .args(["clone", &url, &agentfs_path_clone])
                .output()
                .await;
            let new_status = match output {
                Ok(o) if o.status.success() => "ready",
                _ => "failed",
            };
            // Update clone_status in projects list
            let mut projects = read_projects(&state_clone).await;
            if let Some(p) = projects.iter_mut().find(|p| {
                p.get("id").and_then(|v| v.as_str()) == Some(&id_clone)
            }) {
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("clone_status".to_string(), json!(new_status));
                    obj.insert("updatedAt".to_string(), json!(chrono::Utc::now().to_rfc3339()));
                }
            }
            write_projects(&state_clone, projects).await;
        } else {
            // No git_url — just ensure the directory exists
            let _ = tokio::fs::create_dir_all(&agentfs_path_clone).await;
        }
    });

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
            for field in &["name", "description", "repoUrl", "slackChannels", "tags", "status",
                           "git_url", "slug", "agentfs_path", "clone_status",
                           "owner", "assignee", "notes"] {
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
//
// ?hard=true  — physically remove from storage and delete agentfs_path on disk.
// Default (no param) — soft-archive: sets status="archived", keeps the record.

async fn delete_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let hard = params.get("hard").map(|v| v == "true").unwrap_or(false);
    let force = params.get("force").map(|v| v == "true").unwrap_or(false);
    let projects = read_projects(&state).await;
    let idx = projects.iter().position(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(&id)
    });
    match idx {
        None => (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"Project not found"}))).into_response(),
        Some(i) => {
            // Dirty-bit gate: refuse delete (soft or hard) when AgentFS
            // has unpushed changes, unless force=true is set. Caller is
            // expected to either (a) run the milestone-commit task to
            // push and mark clean, or (b) explicitly accept loss with
            // force=true.
            let agentfs_dirty = projects[i]
                .get("agentfs_dirty").and_then(|v| v.as_bool()).unwrap_or(false);
            if agentfs_dirty && !force {
                return (
                    axum::http::StatusCode::CONFLICT,
                    Json(json!({
                        "error": "agentfs_dirty",
                        "message": "Project has unpushed AgentFS changes. Run the milestone-commit task to push, or pass force=true to discard.",
                        "project_id": id,
                    })),
                ).into_response();
            }
            let mut projects = projects;
            if hard {
                let removed = projects.remove(i);
                write_projects(&state, projects).await;
                // Best-effort cleanup of agentfs workspace directory
                if let Some(path) = removed.get("agentfs_path").and_then(|v| v.as_str()) {
                    let _ = tokio::fs::remove_dir_all(path).await;
                }
                (axum::http::StatusCode::OK, Json(json!({"ok": true, "deleted": removed}))).into_response()
            } else {
                let p = projects[i].as_object_mut().unwrap();
                p.insert("status".to_string(), json!("archived"));
                p.insert("updatedAt".to_string(), json!(chrono::Utc::now().to_rfc3339()));
                let archived = projects[i].clone();
                write_projects(&state, projects).await;
                (axum::http::StatusCode::OK, Json(json!({"ok": true, "project": archived}))).into_response()
            }
        }
    }
}

// ── Beads import core (shared by HTTP handler and background scanner) ─────

/// Import open beads from a project's AgentFS path into fleet_tasks.
/// Returns (imported_count, skipped_count).
/// Idempotent: existing active tasks with matching titles are skipped.
pub async fn import_project_beads_inner(state: &AppState, project: &Value) -> (usize, usize) {
    let agentfs_path = match project.get("agentfs_path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return (0, 0),
    };
    let project_ref = project.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if project_ref.is_empty() { return (0, 0); }

    let issues_path = format!("{}/.beads/issues.jsonl", agentfs_path);
    let content = match tokio::fs::read_to_string(&issues_path).await {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };

    let import_statuses = ["open", "in_progress", "in-progress", "blocked"];
    let assignee = project.get("assignee").and_then(|v| v.as_str()).unwrap_or("all").to_string();

    let map_priority = |issue: &Value| -> i64 {
        issue.get("priority").and_then(|v| v.as_i64()).unwrap_or(2).clamp(0, 4)
    };
    let map_tags = |issue: &Value| -> Value {
        let mut tags: Vec<String> = issue.get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|t| t.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        if let Some(t) = issue.get("issue_type").and_then(|v| v.as_str()) {
            if t != "task" && !tags.contains(&t.to_string()) { tags.push(t.to_string()); }
        }
        if !tags.contains(&"beads".to_string()) { tags.push("beads".to_string()); }
        json!(tags)
    };
    let norm = |s: &str| s.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");

    let now = chrono::Utc::now().to_rfc3339();
    let mut imported = 0usize;
    let mut skipped = 0usize;

    let db = state.fleet_db.lock().await;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let issue: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let status = issue.get("status").and_then(|v| v.as_str()).unwrap_or("open");
        if !import_statuses.contains(&status) { continue; }

        let title = issue.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let beads_id = issue.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if title.is_empty() || beads_id.is_empty() { continue; }

        let title_norm = norm(&title);
        let is_dup: bool = db.query_row(
            "SELECT COUNT(*) FROM fleet_tasks WHERE project_id=?1 AND status IN ('open','claimed','in_progress') AND LOWER(TRIM(title))=?2",
            rusqlite::params![project_ref, title_norm],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0) > 0;
        if is_dup { skipped += 1; continue; }

        let priority = map_priority(&issue);
        let tags = map_tags(&issue);
        let mut description = issue.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if description.len() < 20 {
            description = format!("{} (imported from beads {})", description, beads_id);
        }
        let task_id = format!("task-beads-{}-{}", beads_id, chrono::Utc::now().timestamp_millis());
        let issue_type = issue.get("issue_type").and_then(|v| v.as_str()).unwrap_or("task").to_string();
        let metadata = json!({
            "beads_id": beads_id,
            "source": "beads-scanner",
            "tags": tags,
            "assignee": assignee,
        });

        if db.execute(
            "INSERT INTO fleet_tasks (id, project_id, title, description, priority, task_type, metadata, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            rusqlite::params![task_id, project_ref, title, description, priority, issue_type, metadata.to_string(), now],
        ).is_ok() {
            let _ = state.bus_tx.send(json!({
                "type": "tasks:added",
                "task_id": task_id,
                "project_id": project_ref,
                "beads_id": beads_id,
            }).to_string());
            imported += 1;
        } else {
            skipped += 1;
        }
    }

    (imported, skipped)
}

/// Background scanner: runs every BEADS_SCAN_INTERVAL_SECS (default 60).
/// Scans all active projects with an agentfs_path for new beads and imports them.
pub async fn run_beads_scanner(state: Arc<AppState>) {
    let interval_secs: u64 = std::env::var("BEADS_SCAN_INTERVAL_SECS")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(60);
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tracing::info!("beads-scanner: started (interval={}s)", interval_secs);
    loop {
        interval.tick().await;
        let projects = read_projects(&state).await;
        let active: Vec<Value> = projects.into_iter().filter(|p| {
            p.get("status").and_then(|v| v.as_str()) != Some("archived")
            && p.get("clone_status").and_then(|v| v.as_str()) == Some("ready")
            && p.get("agentfs_path").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false)
        }).collect();
        let mut total_imported = 0usize;
        for project in &active {
            let (n, _) = import_project_beads_inner(&state, project).await;
            total_imported += n;
        }
        if total_imported > 0 {
            tracing::info!("beads-scanner: imported {} new task(s) across {} project(s)", total_imported, active.len());
        }
    }
}

// ── POST /api/projects/:id/import-beads ───────────────────────────────────
//
// Reads .beads/issues.jsonl from the project's agentfs_path and creates a
// queue task for each open issue (status: open | in_progress | blocked).
//
// Query params:
//   status=open,in_progress   — comma-separated statuses to import (default above)
//   assignee=<agent>          — override assignee on created tasks (default: project assignee or "all")
//   dry_run=true              — parse and report without writing to queue
//
// Returns:
//   { "imported": N, "skipped": N, "tasks": [...] }

async fn import_beads(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    // Find the project
    let projects = read_projects(&state).await;
    let project = match projects.iter().find(|p| {
        p.get("id").and_then(|v| v.as_str()) == Some(&id)
            || p.get("slug").and_then(|v| v.as_str()) == Some(&id)
    }) {
        Some(p) => p.clone(),
        None => return (axum::http::StatusCode::NOT_FOUND, Json(json!({"error": "Project not found"}))).into_response(),
    };

    let agentfs_path = match project.get("agentfs_path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return (axum::http::StatusCode::BAD_REQUEST, Json(json!({
            "error": "Project has no agentfs_path set"
        }))).into_response(),
    };

    // dry_run mode: parse and report without writing
    let dry_run = params.get("dry_run").map(|v| v == "true").unwrap_or(false);
    if dry_run {
        let import_statuses: Vec<&str> = vec!["open", "in_progress", "in-progress", "blocked"];
        let issues_path = format!("{}/.beads/issues.jsonl", agentfs_path);
        let content = tokio::fs::read_to_string(&issues_path).await.unwrap_or_default();
        let to_import: Vec<Value> = content.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
            .filter(|v| import_statuses.contains(&v.get("status").and_then(|s| s.as_str()).unwrap_or("open")))
            .collect();
        return (axum::http::StatusCode::OK, Json(json!({
            "dry_run": true,
            "would_import": to_import.len(),
            "issues": to_import,
        }))).into_response();
    }

    let (imported, skipped) = import_project_beads_inner(&state, &project).await;

    (axum::http::StatusCode::OK, Json(json!({
        "ok":      true,
        "imported": imported,
        "skipped":  skipped,
    }))).into_response()
}
