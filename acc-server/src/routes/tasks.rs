//! Global fleet task pool — single source of truth for multi-agent work coordination.
//!
//! Agents poll GET /api/tasks?status=open, then atomically claim via PUT /api/tasks/:id/claim.
//! The SQL WHERE-clause ensures only one agent wins a race; losers get 409.
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
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
}

#[derive(Deserialize)]
struct TaskQuery {
    status: Option<String>,
    project_id: Option<String>,
    agent: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let metadata_str: String = row.get(12)?;
    let metadata: Value = serde_json::from_str(&metadata_str).unwrap_or(json!({}));
    Ok(json!({
        "id":             row.get::<_, String>(0)?,
        "project_id":     row.get::<_, String>(1)?,
        "title":          row.get::<_, String>(2)?,
        "description":    row.get::<_, String>(3)?,
        "status":         row.get::<_, String>(4)?,
        "priority":       row.get::<_, i64>(5)?,
        "claimed_by":     row.get::<_, Option<String>>(6)?,
        "claimed_at":     row.get::<_, Option<String>>(7)?,
        "claim_expires_at": row.get::<_, Option<String>>(8)?,
        "completed_at":   row.get::<_, Option<String>>(9)?,
        "completed_by":   row.get::<_, Option<String>>(10)?,
        "created_at":     row.get::<_, String>(11)?,
        "metadata":       metadata,
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
    let mut sql = String::from(
        "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,claim_expires_at,completed_at,completed_by,created_at,metadata FROM fleet_tasks WHERE 1=1"
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
        "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,claim_expires_at,completed_at,completed_by,created_at,metadata FROM fleet_tasks WHERE id=?1",
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
    let metadata = body.get("metadata").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());

    let db = state.fleet_db.lock().await;
    match db.execute(
        "INSERT INTO fleet_tasks (id, project_id, title, description, priority, metadata) VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, project_id, title, description, priority, metadata],
    ) {
        Ok(_) => {
            let task = db.query_row(
                "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,claim_expires_at,completed_at,completed_by,created_at,metadata FROM fleet_tasks WHERE id=?1",
                params![id],
                row_to_task,
            ).unwrap_or(json!({"id": id}));
            // Broadcast on bus
            let _ = state.bus_tx.send(json!({"type":"tasks:added","task_id":id,"project_id":project_id}).to_string());
            (StatusCode::CREATED, Json(json!({"ok":true,"task":task}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
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
    let task = db.query_row(
        "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,claim_expires_at,completed_at,completed_by,created_at,metadata FROM fleet_tasks WHERE id=?1",
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
    // Max concurrent tasks per agent (default 3, configurable)
    let max_tasks: i64 = std::env::var("ACC_MAX_TASKS_PER_AGENT")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(3);

    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();
    // 4-hour claim lease
    let expires_str = (now + chrono::Duration::hours(4)).to_rfc3339();

    // Check agent's current load
    let active: i64 = db.query_row(
        "SELECT COUNT(*) FROM fleet_tasks WHERE claimed_by=?1 AND status IN ('claimed','in_progress')",
        params![agent],
        |r| r.get(0),
    ).unwrap_or(0);

    if active >= max_tasks {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({
            "error": "agent_at_capacity",
            "active": active,
            "max": max_tasks,
        }))).into_response();
    }

    // Atomic claim: only succeeds if still open
    let rows = db.execute(
        "UPDATE fleet_tasks SET status='claimed', claimed_by=?1, claimed_at=?2, claim_expires_at=?3, updated_at=?2 WHERE id=?4 AND status='open'",
        params![agent, now_str, expires_str, id],
    ).unwrap_or(0);

    if rows == 0 {
        // Either not found or already claimed
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
        "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,claim_expires_at,completed_at,completed_by,created_at,metadata FROM fleet_tasks WHERE id=?1",
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
        "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,claim_expires_at,completed_at,completed_by,created_at,metadata FROM fleet_tasks WHERE id=?1",
        params![id], row_to_task,
    ).unwrap_or(json!({"id":id}));
    let _ = state.bus_tx.send(json!({"type":"tasks:completed","task_id":id,"agent":agent}).to_string());
    Json(json!({"ok":true,"task":task})).into_response()
}
