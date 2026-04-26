//! Global fleet task pool — single source of truth for multi-agent work coordination.
//!
//! Agents poll GET /api/tasks?status=open, then atomically claim via PUT /api/tasks/:id/claim.
//! The SQL WHERE-clause ensures only one agent wins a race; losers get 409.
//! Tasks with blocked_by dependencies return 423 (Locked) until all blockers complete+approved.
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route("/api/tasks/:id", get(get_task).put(update_task).delete(cancel_task))
        .route("/api/tasks/:id/claim", put(claim_task))
        .route("/api/tasks/:id/unclaim", put(unclaim_task))
        .route("/api/tasks/:id/complete", put(complete_task))
        .route("/api/tasks/:id/review-result", put(set_review_result))
        .route("/api/tasks/:id/vote", put(vote_on_task))
}

#[derive(Deserialize)]
struct TaskQuery {
    status: Option<String>,
    project_id: Option<String>,
    agent: Option<String>,
    task_type: Option<String>,
    phase: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

// Columns 0-12 (original) + 13-17 (new)
const TASK_COLS: &str = "id,project_id,title,description,status,priority,claimed_by,claimed_at,\
    claim_expires_at,completed_at,completed_by,created_at,metadata,\
    task_type,review_of,phase,blocked_by,review_result";

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let metadata_str: String = row.get(12)?;
    let metadata: Value = serde_json::from_str(&metadata_str).unwrap_or(json!({}));
    let blocked_by_str: String = row.get(16).unwrap_or_else(|_| "[]".to_string());
    let blocked_by: Value = serde_json::from_str(&blocked_by_str).unwrap_or(json!([]));
    Ok(json!({
        "id":               row.get::<_, String>(0)?,
        "project_id":       row.get::<_, String>(1)?,
        "title":            row.get::<_, String>(2)?,
        "description":      row.get::<_, String>(3)?,
        "status":           row.get::<_, String>(4)?,
        "priority":         row.get::<_, i64>(5)?,
        "claimed_by":       row.get::<_, Option<String>>(6)?,
        "claimed_at":       row.get::<_, Option<String>>(7)?,
        "claim_expires_at": row.get::<_, Option<String>>(8)?,
        "completed_at":     row.get::<_, Option<String>>(9)?,
        "completed_by":     row.get::<_, Option<String>>(10)?,
        "created_at":       row.get::<_, String>(11)?,
        "metadata":         metadata,
        "task_type":        row.get::<_, String>(13).unwrap_or_else(|_| "work".to_string()),
        "review_of":        row.get::<_, Option<String>>(14)?,
        "phase":            row.get::<_, Option<String>>(15)?,
        "blocked_by":       blocked_by,
        "review_result":    row.get::<_, Option<String>>(17)?,
    }))
}

async fn list_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TaskQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let db = state.fleet_db.lock().await;
    let mut sql = format!(
        "SELECT {TASK_COLS} FROM fleet_tasks WHERE 1=1"
    );
    let mut binds: Vec<String> = vec![];

    if let Some(s) = &q.status {
        sql.push_str(" AND status=?");
        binds.push(s.clone());
    }
    if let Some(p) = &q.project_id {
        sql.push_str(" AND project_id=?");
        binds.push(p.clone());
    }
    if let Some(a) = &q.agent {
        sql.push_str(" AND claimed_by=?");
        binds.push(a.clone());
    }
    if let Some(tt) = &q.task_type {
        sql.push_str(" AND task_type=?");
        binds.push(tt.clone());
    }
    if let Some(ph) = &q.phase {
        sql.push_str(" AND phase=?");
        binds.push(ph.clone());
    }
    sql.push_str(" ORDER BY priority ASC, created_at ASC");
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

    let mut stmt = match db.prepare(&sql) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    let tasks: Vec<Value> = match stmt
        .query_map(rusqlite::params_from_iter(binds.iter().map(|s| s.as_str())), row_to_task)
    {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    };

    let count = tasks.len();
    Json(json!({"tasks": tasks, "count": count})).into_response()
}

async fn get_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let db = state.fleet_db.lock().await;
    let result = db.query_row(
        &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
        params![id],
        row_to_task,
    );
    match result {
        Ok(task) => Json(task).into_response(),
        Err(rusqlite::Error::QueryReturnedNoRows) => (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

async fn create_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let project_id = match body.get("project_id").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error":"project_id required"}))).into_response(),
    };
    let title = match body.get("title").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error":"title required"}))).into_response(),
    };
    let id = format!("task-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let description = body.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let priority = body.get("priority").and_then(|v| v.as_i64()).unwrap_or(2);
    let task_type = body.get("task_type").and_then(|v| v.as_str()).unwrap_or("work").to_string();
    // For idea tasks, record creator so they can't vote on their own idea.
    let metadata = {
        let mut m: serde_json::Value = body.get("metadata")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if task_type == "idea" {
            if let Some(agent) = body.get("agent").and_then(|v| v.as_str()) {
                m["created_by"] = serde_json::json!(agent);
            }
        }
        m.to_string()
    };
    let review_of: Option<String> = body.get("review_of").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string());
    let phase: String = body.get("phase").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()).unwrap_or_else(|| "build".to_string());
    let blocked_by = body.get("blocked_by")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "[]".to_string());

    // Use a block so db (MutexGuard, !Send) is dropped before any .await
    let insert_result: Result<Value, String> = {
        let db = state.fleet_db.lock().await;
        match db.execute(
            "INSERT INTO fleet_tasks (id,project_id,title,description,priority,metadata,task_type,review_of,phase,blocked_by)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![id, project_id, title, description, priority, metadata, task_type, review_of, phase, blocked_by],
        ) {
            Ok(_) => {
                let task = db.query_row(
                    &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
                    params![id],
                    row_to_task,
                ).unwrap_or(json!({"id": id}));
                let _ = state.bus_tx.send(json!({"type":"tasks:added","task_id":id,"project_id":project_id}).to_string());
                Ok(task)
            }
            Err(e) => Err(e.to_string()),
        }
    }; // db dropped here

    match insert_result {
        Ok(task) => {
            crate::dispatch::nudge_new_task(&state, &task).await;
            (StatusCode::CREATED, Json(json!({"ok":true,"task":task}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}

async fn update_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(title) = body.get("title").and_then(|v| v.as_str()) {
        let _ = db.execute("UPDATE fleet_tasks SET title=?1, updated_at=?2 WHERE id=?3", params![title, now, id]);
    }
    if let Some(desc) = body.get("description").and_then(|v| v.as_str()) {
        let _ = db.execute("UPDATE fleet_tasks SET description=?1, updated_at=?2 WHERE id=?3", params![desc, now, id]);
    }
    if let Some(p) = body.get("priority").and_then(|v| v.as_i64()) {
        let _ = db.execute("UPDATE fleet_tasks SET priority=?1, updated_at=?2 WHERE id=?3", params![p, now, id]);
    }
    if let Some(m) = body.get("metadata") {
        let _ = db.execute("UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3", params![m.to_string(), now, id]);
    }
    if let Some(s) = body.get("status").and_then(|v| v.as_str()) {
        let _ = db.execute("UPDATE fleet_tasks SET status=?1, updated_at=?2 WHERE id=?3", params![s, now, id]);
    }
    if let Some(tt) = body.get("task_type").and_then(|v| v.as_str()) {
        let _ = db.execute("UPDATE fleet_tasks SET task_type=?1, updated_at=?2 WHERE id=?3", params![tt, now, id]);
    }
    if let Some(ph) = body.get("phase").and_then(|v| v.as_str()) {
        let _ = db.execute("UPDATE fleet_tasks SET phase=?1, updated_at=?2 WHERE id=?3", params![ph, now, id]);
    }
    if let Some(bb) = body.get("blocked_by") {
        let _ = db.execute("UPDATE fleet_tasks SET blocked_by=?1, updated_at=?2 WHERE id=?3", params![bb.to_string(), now, id]);
    }
    let task = db.query_row(
        &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
        params![id], row_to_task,
    );
    match task {
        Ok(t) => Json(json!({"ok":true,"task":t})).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found"}))).into_response(),
    }
}

async fn cancel_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    match db.execute("UPDATE fleet_tasks SET status='cancelled', updated_at=?1 WHERE id=?2", params![now, id]) {
        Ok(0) => (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found"}))).into_response(),
        Ok(_) => Json(json!({"ok":true})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":e.to_string()}))).into_response(),
    }
}

async fn claim_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let agent = match body.get("agent").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error":"agent required"}))).into_response(),
    };
    let max_tasks: i64 = std::env::var("ACC_MAX_TASKS_PER_AGENT")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(3);

    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();
    let expires_str = (now + chrono::Duration::hours(4)).to_rfc3339();

    // Determine what type of task is being claimed (for per-type capacity logic)
    let task_type: String = db.query_row(
        "SELECT task_type FROM fleet_tasks WHERE id=?1",
        params![id],
        |r| r.get(0),
    ).unwrap_or_else(|_| "work".to_string());

    // Check agent's current load. Review tasks get 1 extra slot beyond the work cap
    // so that agents at work capacity can still drain the review queue.
    let active_work: i64 = db.query_row(
        "SELECT COUNT(*) FROM fleet_tasks WHERE claimed_by=?1 AND status IN ('claimed','in_progress') \
         AND task_type NOT IN ('review','phase_commit')",
        params![agent],
        |r| r.get(0),
    ).unwrap_or(0);
    let active_review: i64 = db.query_row(
        "SELECT COUNT(*) FROM fleet_tasks WHERE claimed_by=?1 AND status IN ('claimed','in_progress') \
         AND task_type = 'review'",
        params![agent],
        |r| r.get(0),
    ).unwrap_or(0);

    let at_capacity = if task_type == "review" {
        // Reviews: blocked only when the review slot is already taken (1 concurrent review)
        active_review >= 1
    } else {
        // Work/phase_commit/discovery: blocked at max_tasks
        active_work >= max_tasks
    };

    if at_capacity {
        let active = active_work + active_review;
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({
            "error": "agent_at_capacity",
            "active": active,
            "max": max_tasks,
        }))).into_response();
    }

    // Check dependency blocking: all blocked_by tasks must be completed+approved
    let blocked_by_str: String = db.query_row(
        "SELECT blocked_by FROM fleet_tasks WHERE id=?1",
        params![id],
        |r| r.get(0),
    ).unwrap_or_else(|_| "[]".to_string());
    let blocked_by: Vec<String> = serde_json::from_str(&blocked_by_str).unwrap_or_default();

    for blocker_id in &blocked_by {
        if blocker_id.is_empty() { continue; }
        let satisfied: bool = db.query_row(
            "SELECT COUNT(*) FROM fleet_tasks WHERE id=?1 AND status='completed' \
             AND (review_result IS NULL OR review_result != 'rejected')",
            params![blocker_id],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0) > 0;
        if !satisfied {
            return (StatusCode::LOCKED, Json(json!({"error":"blocked","pending":blocker_id}))).into_response();
        }
    }

    // Atomic claim: only succeeds if still open
    let rows = db.execute(
        "UPDATE fleet_tasks SET status='claimed', claimed_by=?1, claimed_at=?2, claim_expires_at=?3, updated_at=?2 WHERE id=?4 AND status='open'",
        params![agent, now_str, expires_str, id],
    ).unwrap_or(0);

    if rows == 0 {
        let exists: bool = db.query_row(
            "SELECT COUNT(*) FROM fleet_tasks WHERE id=?1", params![id], |r| r.get::<_,i64>(0)
        ).unwrap_or(0) > 0;
        return if exists {
            (StatusCode::CONFLICT, Json(json!({"error":"already_claimed"}))).into_response()
        } else {
            (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found"}))).into_response()
        };
    }

    let task = db.query_row(
        &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
        params![id], row_to_task,
    ).unwrap_or(json!({"id":id}));

    let _ = state.bus_tx.send(json!({"type":"tasks:claimed","task_id":id,"agent":agent}).to_string());
    (StatusCode::OK, Json(json!({"ok":true,"task":task}))).into_response()
}

async fn unclaim_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let agent = body.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    let rows = db.execute(
        "UPDATE fleet_tasks SET status='open', claimed_by=NULL, claimed_at=NULL, claim_expires_at=NULL, updated_at=?1 WHERE id=?2 AND (claimed_by=?3 OR ?3='')",
        params![now, id, agent],
    ).unwrap_or(0);
    if rows == 0 {
        return (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found or not owned by agent"}))).into_response();
    }
    let _ = state.bus_tx.send(json!({"type":"tasks:unclaimed","task_id":id,"agent":agent}).to_string());
    Json(json!({"ok":true})).into_response()
}

async fn complete_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let agent = body.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    let rows = db.execute(
        "UPDATE fleet_tasks SET status='completed', completed_at=?1, completed_by=?2, claim_expires_at=NULL, updated_at=?1 WHERE id=?3 AND status IN ('claimed','in_progress','open')",
        params![now, agent, id],
    ).unwrap_or(0);
    if rows == 0 {
        return (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found"}))).into_response();
    }
    let task = db.query_row(
        &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
        params![id], row_to_task,
    ).unwrap_or(json!({"id":id}));
    drop(db); // release lock before potentially calling into projects state
    let _ = state.bus_tx.send(json!({"type":"tasks:completed","task_id":id,"agent":agent}).to_string());

    // CCC-tk0 / drift-fix #3: any completed task may have modified the
    // project's AgentFS workspace. Mark the project dirty ONLY if the
    // workspace actually has uncommitted changes — agents whose writes
    // silently fail (e.g., bullwinkle TCC) shouldn't trigger a phase
    // commit of an unchanged tree. The milestone-commit task itself
    // calls POST /api/projects/:id/clean so it doesn't re-mark its own
    // project dirty.
    if let Some(pid) = task.get("project_id").and_then(|v| v.as_str()).map(String::from) {
        let task_type = task.get("task_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !pid.is_empty() && task_type != "phase_commit" {
            let state_clone = state.clone();
            tokio::spawn(async move {
                let projects = state_clone.projects.read().await;
                let agentfs_path = projects.iter()
                    .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(pid.as_str()))
                    .and_then(|p| p.get("agentfs_path").and_then(|v| v.as_str()))
                    .map(String::from);
                drop(projects);
                let actually_dirty = match agentfs_path.as_deref() {
                    Some(p) if std::path::Path::new(p).join(".git").exists() => {
                        match tokio::process::Command::new("git")
                            .args(["-C", p, "status", "--porcelain"])
                            .output().await
                        {
                            Ok(o) if o.status.success() => !o.stdout.is_empty(),
                            // If the git command fails (e.g., not a checkout
                            // on this host), trust the agent and mark dirty.
                            _ => true,
                        }
                    }
                    // No accessible agentfs path: fall back to the old
                    // behavior of trusting the task signal.
                    _ => true,
                };
                if actually_dirty {
                    let _ = crate::routes::projects::set_agentfs_dirty(&state_clone, &pid, true).await;
                } else {
                    tracing::info!(
                        component = "tasks",
                        project = %pid,
                        "task completed but git status --porcelain is empty — not marking dirty (likely silent write failure)",
                    );
                }
            });
        }
    }

    // F4: notify GitHub if this task has a linked issue
    if let Some(meta) = task.get("metadata").and_then(|m| m.as_object()) {
        if let (Some(gh_num), Some(gh_repo)) = (
            meta.get("github_number").and_then(|v| v.as_i64()),
            meta.get("github_repo").and_then(|v| v.as_str()).map(|s| s.to_string()),
        ) {
            let task_id_clone = id.clone();
            let agent_clone = agent.clone();
            let auto_close = std::env::var("GITHUB_AUTO_CLOSE").unwrap_or_default() == "true";
            tokio::spawn(async move {
                notify_github_issue(&gh_repo, gh_num as u64, &task_id_clone, &agent_clone, auto_close).await;
            });
        }
    }

    Json(json!({"ok":true,"task":task})).into_response()
}

async fn set_review_result(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let result = match body.get("result").and_then(|v| v.as_str()) {
        Some(r) if r == "approved" || r == "rejected" => r.to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error":"result must be 'approved' or 'rejected'"}))).into_response(),
    };
    let agent = body.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let notes = body.get("notes").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();

    // Merge notes into existing metadata
    let current_meta: String = db.query_row(
        "SELECT metadata FROM fleet_tasks WHERE id=?1",
        params![id],
        |r| r.get(0),
    ).unwrap_or_else(|_| "{}".to_string());
    let mut meta: Value = serde_json::from_str(&current_meta).unwrap_or(json!({}));
    if !notes.is_empty() {
        meta["review_notes"] = Value::String(notes);
    }

    let rows = db.execute(
        "UPDATE fleet_tasks SET review_result=?1, metadata=?2, updated_at=?3 WHERE id=?4",
        params![result, meta.to_string(), now, id],
    ).unwrap_or(0);

    if rows == 0 {
        return (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found"}))).into_response();
    }

    let event_type = if result == "approved" { "tasks:review_approved" } else { "tasks:review_rejected" };
    let _ = state.bus_tx.send(json!({"type":event_type,"task_id":id,"agent":agent}).to_string());
    Json(json!({"ok":true})).into_response()
}

async fn vote_on_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    let agent = match body.get("agent").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(a) => a.to_string(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error":"agent required"}))).into_response(),
    };
    let vote = match body.get("vote").and_then(|v| v.as_str()) {
        Some(v) if v == "approve" || v == "reject" => v.to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error":"vote must be 'approve' or 'reject'"}))).into_response(),
    };
    let refinement = match body.get("refinement").and_then(|v| v.as_str()) {
        Some(r) if !r.trim().is_empty() => r.trim().to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error":"refinement required"}))).into_response(),
    };

    let db = state.fleet_db.lock().await;

    // Fetch task
    let (task_type, current_meta): (String, String) = match db.query_row(
        "SELECT task_type, metadata FROM fleet_tasks WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(row) => row,
        Err(_) => return (StatusCode::NOT_FOUND, Json(json!({"error":"Task not found"}))).into_response(),
    };

    if task_type != "idea" {
        return (StatusCode::CONFLICT, Json(json!({"error":"task is not an idea"}))).into_response();
    }

    let mut meta: Value = serde_json::from_str(&current_meta).unwrap_or(json!({}));

    // Prevent self-voting
    let creator = meta["created_by"].as_str().unwrap_or("").to_string();
    if !creator.is_empty() && creator == agent {
        return (StatusCode::CONFLICT, Json(json!({"error":"cannot vote on own idea"}))).into_response();
    }

    // Ensure votes array exists
    if !meta["votes"].is_array() {
        meta["votes"] = json!([]);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let votes = meta["votes"].as_array_mut().expect("votes is array");
    if let Some(existing) = votes.iter_mut().find(|v| v["agent"].as_str() == Some(&agent)) {
        existing["vote"] = json!(vote);
        existing["refinement"] = json!(refinement);
        existing["voted_at"] = json!(now);
    } else {
        votes.push(json!({
            "agent": agent,
            "vote": vote,
            "refinement": refinement,
            "voted_at": now,
        }));
    }

    let rows = db.execute(
        "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
        params![meta.to_string(), now, id],
    ).unwrap_or(0);

    if rows == 0 {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"update failed"}))).into_response();
    }

    let task = db.query_row(
        &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
        params![id], row_to_task,
    ).unwrap_or(json!({"id": id}));

    let _ = state.bus_tx.send(json!({
        "type": "tasks:voted",
        "task_id": id,
        "agent": agent,
        "vote": vote,
    }).to_string());

    Json(json!({"ok":true,"task":task})).into_response()
}

async fn notify_github_issue(repo: &str, number: u64, task_id: &str, agent: &str, auto_close: bool) {
    let comment = format!(
        "✅ Fleet task `{}` completed by agent `{}`.\n\nThis issue has been resolved by the ACC agent fleet.",
        task_id, agent
    );
    let comment_status = tokio::process::Command::new("gh")
        .args(["issue", "comment", &number.to_string(), "--repo", repo, "--body", &comment])
        .output()
        .await;
    match comment_status {
        Ok(out) if out.status.success() =>
            tracing::info!("github: commented on {}#{}", repo, number),
        Ok(out) =>
            tracing::warn!("github: comment on {}#{} failed: {}", repo, number,
                String::from_utf8_lossy(&out.stderr).trim()),
        Err(e) =>
            tracing::warn!("github: gh CLI not available: {}", e),
    }
    if auto_close {
        let close_status = tokio::process::Command::new("gh")
            .args(["issue", "close", &number.to_string(), "--repo", repo])
            .output()
            .await;
        match close_status {
            Ok(out) if out.status.success() =>
                tracing::info!("github: closed {}#{}", repo, number),
            Ok(out) =>
                tracing::warn!("github: close {}#{} failed: {}", repo, number,
                    String::from_utf8_lossy(&out.stderr).trim()),
            Err(e) =>
                tracing::warn!("github: gh CLI close failed: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{self, body_json, post_json, TestServer};
    use axum::http::Request;
    use axum::body::Body;

    fn put_json(path: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(path)
            .header("Authorization", format!("Bearer {}", testing::TEST_TOKEN))
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn create_idea(server: &TestServer, project: &str, title: &str, creator: &str) -> Value {
        let resp = testing::call(&server.app, post_json("/api/tasks", &json!({
            "project_id": project,
            "title": title,
            "task_type": "idea",
            "agent": creator,
        }))).await;
        let body = body_json(resp).await;
        body["task"].clone()
    }

    #[tokio::test]
    async fn test_vote_approve_stores_vote() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        let resp = testing::call(&server.app, put_json(
            &format!("/api/tasks/{}/vote", task_id),
            &json!({"agent":"bob","vote":"approve","refinement":"Great idea, add error handling"}),
        )).await;
        let body = body_json(resp).await;
        assert_eq!(body["ok"], json!(true));
        let votes = body["task"]["metadata"]["votes"].as_array().unwrap();
        assert_eq!(votes.len(), 1);
        assert_eq!(votes[0]["agent"], json!("bob"));
        assert_eq!(votes[0]["vote"], json!("approve"));
    }

    #[tokio::test]
    async fn test_vote_missing_refinement_rejected() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        let resp = testing::call(&server.app, put_json(
            &format!("/api/tasks/{}/vote", task_id),
            &json!({"agent":"bob","vote":"approve"}),
        )).await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_vote_empty_refinement_rejected() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        let resp = testing::call(&server.app, put_json(
            &format!("/api/tasks/{}/vote", task_id),
            &json!({"agent":"bob","vote":"approve","refinement":"   "}),
        )).await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_vote_self_vote_rejected() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        let resp = testing::call(&server.app, put_json(
            &format!("/api/tasks/{}/vote", task_id),
            &json!({"agent":"alice","vote":"approve","refinement":"I like my own idea"}),
        )).await;
        assert_eq!(resp.status(), 409);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("own idea"));
    }

    #[tokio::test]
    async fn test_vote_on_non_idea_rejected() {
        let server = TestServer::new().await;
        let resp = testing::call(&server.app, post_json("/api/tasks", &json!({
            "project_id": "proj-a",
            "title": "A work task",
            "task_type": "work",
        }))).await;
        let body = body_json(resp).await;
        let task_id = body["task"]["id"].as_str().unwrap();

        let resp = testing::call(&server.app, put_json(
            &format!("/api/tasks/{}/vote", task_id),
            &json!({"agent":"bob","vote":"approve","refinement":"looks good"}),
        )).await;
        assert_eq!(resp.status(), 409);
    }

    #[tokio::test]
    async fn test_vote_updates_existing_vote() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        // First vote
        testing::call(&server.app, put_json(
            &format!("/api/tasks/{}/vote", task_id),
            &json!({"agent":"bob","vote":"reject","refinement":"Not sure about this"}),
        )).await;

        // Change vote
        let resp = testing::call(&server.app, put_json(
            &format!("/api/tasks/{}/vote", task_id),
            &json!({"agent":"bob","vote":"approve","refinement":"Actually changed my mind, it's good"}),
        )).await;
        let body = body_json(resp).await;
        let votes = body["task"]["metadata"]["votes"].as_array().unwrap();
        assert_eq!(votes.len(), 1, "should not duplicate");
        assert_eq!(votes[0]["vote"], json!("approve"));
    }

    #[tokio::test]
    async fn test_idea_created_by_set() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        assert_eq!(task["metadata"]["created_by"], json!("alice"));
    }
}
