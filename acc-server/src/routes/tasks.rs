//! Global fleet task pool — single source of truth for multi-agent work coordination.
//!
//! Agents poll GET /api/tasks?status=open, then atomically claim via PUT /api/tasks/:id/claim.
//! The SQL WHERE-clause ensures only one agent wins a race; losers get 409.
//! Tasks with blocked_by dependencies return 423 (Locked) until all blockers complete+approved.
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route("/api/tasks/graph", get(get_task_graph))
        .route(
            "/api/tasks/:id",
            get(get_task).put(update_task).delete(cancel_task),
        )
        .route("/api/tasks/:id/claim", put(claim_task))
        .route("/api/tasks/:id/unclaim", put(unclaim_task))
        .route("/api/tasks/:id/complete", put(complete_task))
        .route("/api/tasks/:id/review-result", put(set_review_result))
        .route("/api/tasks/:id/vote", put(vote_on_task))
        .route("/api/tasks/:id/fanout", post(fanout_task))
        .route("/api/tasks/:id/keepalive", put(keepalive_task))
        .route("/api/tasks/:id/turns", get(get_turns).post(append_turn))
}

#[derive(Deserialize)]
struct TaskQuery {
    status: Option<String>,
    project_id: Option<String>,
    agent: Option<String>,
    task_type: Option<String>,
    phase: Option<String>,
    source: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

// Columns 0-12 (original) + 13-17 (new) + 18-19 (output, inputs) + 20 (source)
const TASK_COLS: &str = "id,project_id,title,description,status,priority,claimed_by,claimed_at,\
    claim_expires_at,completed_at,completed_by,created_at,metadata,\
    task_type,review_of,phase,blocked_by,review_result,output,inputs,source";

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: String = row.get(0)?;
    let task_type = row
        .get::<_, String>(13)
        .unwrap_or_else(|_| "work".to_string());
    let metadata_str: String = row.get(12)?;
    let mut metadata: Value = serde_json::from_str(&metadata_str).unwrap_or(json!({}));
    normalize_workflow_metadata(&mut metadata, &id, &task_type);
    let preferred_executor = metadata
        .get("preferred_executor")
        .cloned()
        .unwrap_or(Value::Null);
    let required_executors = metadata
        .get("required_executors")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let preferred_agent = metadata
        .get("preferred_agent")
        .cloned()
        .unwrap_or(Value::Null);
    let assigned_agent = metadata
        .get("assigned_agent")
        .cloned()
        .unwrap_or(Value::Null);
    let assigned_session = metadata
        .get("assigned_session")
        .cloned()
        .unwrap_or(Value::Null);
    let outcome_id = metadata.get("outcome_id").cloned().unwrap_or(Value::Null);
    let workflow_role = metadata
        .get("workflow_role")
        .cloned()
        .unwrap_or(Value::Null);
    let finisher_agent = metadata
        .get("finisher_agent")
        .cloned()
        .unwrap_or(Value::Null);
    let finisher_session = metadata
        .get("finisher_session")
        .cloned()
        .unwrap_or(Value::Null);
    let chain_id = metadata.get("chain_id").cloned().unwrap_or_else(|| {
        metadata
            .get("source_chain_id")
            .cloned()
            .unwrap_or(Value::Null)
    });
    let blocked_by_str: String = row.get(16).unwrap_or_else(|_| "[]".to_string());
    let blocked_by: Value = serde_json::from_str(&blocked_by_str).unwrap_or(json!([]));
    let output_val: Value = {
        let s: Option<String> = row.get(18).unwrap_or(None);
        s.and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)
    };
    let inputs_val: Value = {
        let s: String = row.get(19).unwrap_or_else(|_| "{}".to_string());
        serde_json::from_str(&s).unwrap_or(json!({}))
    };
    let source: String = row.get(20).unwrap_or_else(|_| "fleet".to_string());
    Ok(json!({
        "id":               id,
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
        "preferred_executor": preferred_executor,
        "required_executors": required_executors,
        "preferred_agent":  preferred_agent,
        "assigned_agent":   assigned_agent,
        "assigned_session": assigned_session,
        "outcome_id":       outcome_id,
        "workflow_role":    workflow_role,
        "finisher_agent":   finisher_agent,
        "finisher_session": finisher_session,
        "chain_id":         chain_id,
        "task_type":        task_type,
        "review_of":        row.get::<_, Option<String>>(14)?,
        "phase":            row.get::<_, Option<String>>(15)?,
        "blocked_by":       blocked_by,
        "review_result":    row.get::<_, Option<String>>(17)?,
        "output":           output_val,
        "inputs":           inputs_val,
        "source":           source,
    }))
}

fn normalize_workflow_metadata(metadata: &mut Value, task_id: &str, task_type: &str) {
    if !metadata.is_object() {
        *metadata = json!({});
    }
    let missing_outcome = metadata
        .get("outcome_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .is_none();
    if missing_outcome {
        metadata["outcome_id"] = json!(task_id);
    }

    let missing_role = metadata
        .get("workflow_role")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .is_none();
    if missing_role {
        metadata["workflow_role"] = json!(default_workflow_role(task_type));
    }
}

async fn list_tasks(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TaskQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let db = state.fleet_db.lock().await;
    let mut sql = format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE 1=1");
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
    if let Some(src) = &q.source {
        sql.push_str(" AND source=?");
        binds.push(src.clone());
    }
    sql.push_str(" ORDER BY priority ASC, created_at ASC");
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

    let mut stmt = match db.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let tasks: Vec<Value> = match stmt.query_map(
        rusqlite::params_from_iter(binds.iter().map(|s| s.as_str())),
        row_to_task,
    ) {
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let db = state.fleet_db.lock().await;
    let result = db.query_row(
        &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
        params![id],
        row_to_task,
    );
    match result {
        Ok(task) => Json(task).into_response(),
        Err(rusqlite::Error::QueryReturnedNoRows) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Task not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn create_task(
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
    let project_id = match body.get("project_id").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"project_id required"})),
            )
                .into_response()
        }
    };
    let title = match body.get("title").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"title required"})),
            )
                .into_response()
        }
    };
    let id = format!("task-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let priority = body.get("priority").and_then(|v| v.as_i64()).unwrap_or(2);
    let task_type = body
        .get("task_type")
        .and_then(|v| v.as_str())
        .unwrap_or("work")
        .to_string();
    let workflow_role = body
        .get("workflow_role")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            body.get("metadata")
                .and_then(|m| m.get("workflow_role"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
        .unwrap_or_else(|| default_workflow_role(&task_type).to_string());
    // For idea tasks, record creator so they can't vote on their own idea.
    let metadata = {
        let mut m: serde_json::Value = body
            .get("metadata")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(preferred_executor) = body
            .get("preferred_executor")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            m["preferred_executor"] = serde_json::json!(preferred_executor);
        }
        if let Some(preferred_agent) = body
            .get("preferred_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            m["preferred_agent"] = serde_json::json!(preferred_agent);
        }
        if let Some(assigned_agent) = body
            .get("assigned_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            m["assigned_agent"] = serde_json::json!(assigned_agent);
        }
        if let Some(assigned_session) = body
            .get("assigned_session")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            m["assigned_session"] = serde_json::json!(assigned_session);
        }
        if let Some(outcome_id) = body
            .get("outcome_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            m["outcome_id"] = serde_json::json!(outcome_id);
        } else if m
            .get("outcome_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_none()
        {
            m["outcome_id"] = serde_json::json!(&id);
        }
        m["workflow_role"] = serde_json::json!(workflow_role);
        if let Some(finisher_agent) = body
            .get("finisher_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            m["finisher_agent"] = serde_json::json!(finisher_agent);
        }
        if let Some(finisher_session) = body
            .get("finisher_session")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            m["finisher_session"] = serde_json::json!(finisher_session);
        }
        if let Some(chain_id) = body
            .get("chain_id")
            .or_else(|| body.get("source_chain_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                m.get("chain_id")
                    .or_else(|| m.get("source_chain_id"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
        {
            m["chain_id"] = serde_json::json!(chain_id);
        }
        if let Some(required_executors) = body.get("required_executors").and_then(|v| v.as_array())
        {
            let required: Vec<String> = required_executors
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            if !required.is_empty() {
                m["required_executors"] = serde_json::json!(required);
            }
        }
        if task_type == "idea" {
            if let Some(agent) = body.get("agent").and_then(|v| v.as_str()) {
                m["created_by"] = serde_json::json!(agent);
            }
        }
        m.to_string()
    };
    let review_of: Option<String> = body
        .get("review_of")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let phase: String = body
        .get("phase")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "build".to_string());
    let blocked_by_vec: Vec<String> = body
        .get("blocked_by")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let blocked_by = if blocked_by_vec.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(&blocked_by_vec).unwrap_or_else(|_| "[]".to_string())
    };

    // Cycle detection: check before inserting (separate lock so we can drop before .await).
    if !blocked_by_vec.is_empty() {
        let graph = {
            let db = state.fleet_db.lock().await;
            crate::db::db_all_blocked_by(&db)
        };
        if crate::dag::would_create_cycle(&graph, &id, &blocked_by_vec) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"cycle_detected","message":"Adding these dependencies would create a circular dependency"})),
            ).into_response();
        }
    }

    // Use a block so db (MutexGuard, !Send) is dropped before any .await
    let insert_result: Result<Value, String> = {
        let db = state.fleet_db.lock().await;
        match db.execute(
            "INSERT INTO fleet_tasks (id,project_id,title,description,priority,metadata,task_type,review_of,phase,blocked_by)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![id, project_id, title, description, priority, metadata, task_type, review_of, phase, blocked_by],
        ) {
            Ok(_) => {
                if let Some(chain_id) = task_chain_id_from_metadata(&metadata) {
                    let _ = crate::routes::chains::link_task_to_chain(
                        &db,
                        &chain_id,
                        &id,
                        "spawned",
                        &json!({"source": "task_create"}),
                    );
                }
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(title) = body.get("title").and_then(|v| v.as_str()) {
        let _ = db.execute(
            "UPDATE fleet_tasks SET title=?1, updated_at=?2 WHERE id=?3",
            params![title, now, id],
        );
    }
    if let Some(desc) = body.get("description").and_then(|v| v.as_str()) {
        let _ = db.execute(
            "UPDATE fleet_tasks SET description=?1, updated_at=?2 WHERE id=?3",
            params![desc, now, id],
        );
    }
    if let Some(p) = body.get("priority").and_then(|v| v.as_i64()) {
        let _ = db.execute(
            "UPDATE fleet_tasks SET priority=?1, updated_at=?2 WHERE id=?3",
            params![p, now, id],
        );
    }
    if let Some(m) = body.get("metadata") {
        let _ = db.execute(
            "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
            params![m.to_string(), now, id],
        );
    }
    if let Some(s) = body.get("status").and_then(|v| v.as_str()) {
        let _ = db.execute(
            "UPDATE fleet_tasks SET status=?1, updated_at=?2 WHERE id=?3",
            params![s, now, id],
        );
        let _ = crate::routes::chains::mark_task_status(&db, &id, s);
    }
    if let Some(tt) = body.get("task_type").and_then(|v| v.as_str()) {
        let _ = db.execute(
            "UPDATE fleet_tasks SET task_type=?1, updated_at=?2 WHERE id=?3",
            params![tt, now, id],
        );
    }
    if let Some(ph) = body.get("phase").and_then(|v| v.as_str()) {
        let _ = db.execute(
            "UPDATE fleet_tasks SET phase=?1, updated_at=?2 WHERE id=?3",
            params![ph, now, id],
        );
    }
    if body.get("preferred_executor").is_some()
        || body.get("required_executors").is_some()
        || body.get("preferred_agent").is_some()
        || body.get("assigned_agent").is_some()
        || body.get("assigned_session").is_some()
        || body.get("outcome_id").is_some()
        || body.get("workflow_role").is_some()
        || body.get("finisher_agent").is_some()
        || body.get("finisher_session").is_some()
        || body.get("chain_id").is_some()
        || body.get("source_chain_id").is_some()
    {
        let current_meta: String = db
            .query_row(
                "SELECT metadata FROM fleet_tasks WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "{}".to_string());
        let mut meta: Value = serde_json::from_str(&current_meta).unwrap_or(json!({}));
        maybe_set_meta_string(
            &mut meta,
            "preferred_executor",
            body.get("preferred_executor"),
        );
        maybe_set_meta_string(&mut meta, "preferred_agent", body.get("preferred_agent"));
        maybe_set_meta_string(&mut meta, "assigned_agent", body.get("assigned_agent"));
        maybe_set_meta_string(&mut meta, "assigned_session", body.get("assigned_session"));
        maybe_set_meta_string(&mut meta, "outcome_id", body.get("outcome_id"));
        maybe_set_meta_string(&mut meta, "workflow_role", body.get("workflow_role"));
        maybe_set_meta_string(&mut meta, "finisher_agent", body.get("finisher_agent"));
        maybe_set_meta_string(&mut meta, "finisher_session", body.get("finisher_session"));
        maybe_set_meta_string(&mut meta, "chain_id", body.get("chain_id"));
        maybe_set_meta_string(&mut meta, "source_chain_id", body.get("source_chain_id"));
        if let Some(required_executors) = body.get("required_executors").and_then(|v| v.as_array())
        {
            let required: Vec<String> = required_executors
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            meta["required_executors"] = json!(required);
        }
        let _ = db.execute(
            "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
            params![meta.to_string(), now, id],
        );
        if let Some(chain_id) = meta
            .get("chain_id")
            .or_else(|| meta.get("source_chain_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let _ = crate::routes::chains::link_task_to_chain(
                &db,
                chain_id,
                &id,
                "spawned",
                &json!({"source": "task_update"}),
            );
        }
    }
    if let Some(bb) = body.get("blocked_by") {
        let new_blockers: Vec<String> = bb
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if !new_blockers.is_empty() {
            let graph = crate::db::db_all_blocked_by(&db);
            if crate::dag::would_create_cycle(&graph, &id, &new_blockers) {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error":"cycle_detected","message":"Adding these dependencies would create a circular dependency"})),
                ).into_response();
            }
        }
        let bb_json = serde_json::to_string(&new_blockers).unwrap_or_else(|_| bb.to_string());
        let _ = db.execute(
            "UPDATE fleet_tasks SET blocked_by=?1, updated_at=?2 WHERE id=?3",
            params![bb_json, now, id],
        );
    }
    let task = db.query_row(
        &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
        params![id],
        row_to_task,
    );
    match task {
        Ok(t) => Json(json!({"ok":true,"task":t})).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Task not found"})),
        )
            .into_response(),
    }
}

fn default_workflow_role(task_type: &str) -> &'static str {
    match task_type {
        "review" => "review",
        "phase_commit" => "commit",
        _ => "work",
    }
}

fn maybe_set_meta_string(meta: &mut Value, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        if let Some(s) = value.as_str() {
            if s.is_empty() {
                if let Some(obj) = meta.as_object_mut() {
                    obj.remove(key);
                }
            } else {
                meta[key] = json!(s);
            }
        }
    }
}

fn task_chain_id_from_metadata(metadata: &str) -> Option<String> {
    serde_json::from_str::<Value>(metadata).ok().and_then(|m| {
        m.get("chain_id")
            .or_else(|| m.get("source_chain_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

async fn cancel_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    match db.execute(
        "UPDATE fleet_tasks SET status='cancelled', updated_at=?1 WHERE id=?2",
        params![now, id],
    ) {
        Ok(0) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Task not found"})),
        )
            .into_response(),
        Ok(_) => {
            let _ = crate::routes::chains::mark_task_status(&db, &id, "cancelled");
            Json(json!({"ok":true})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":e.to_string()})),
        )
            .into_response(),
    }
}

async fn claim_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let agent = match body.get("agent").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"agent required"})),
            )
                .into_response()
        }
    };
    let max_tasks: i64 = std::env::var("ACC_MAX_TASKS_PER_AGENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now();
    let now_str = now.to_rfc3339();
    let expires_str = (now + chrono::Duration::hours(4)).to_rfc3339();

    // Determine task semantics before claiming. Assigned-agent and
    // finisher restrictions are authoritative even when old clients poll
    // broadly by task type.
    let (task_type, task_status, metadata_raw): (String, String, String) = match db.query_row(
        "SELECT task_type, status, metadata FROM fleet_tasks WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ) {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Task not found"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":e.to_string()})),
            )
                .into_response()
        }
    };
    if task_status != "open" {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"already_claimed"})),
        )
            .into_response();
    }
    let mut metadata: Value = serde_json::from_str(&metadata_raw).unwrap_or(json!({}));
    normalize_workflow_metadata(&mut metadata, &id, &task_type);
    if let Some(assigned_agent) = metadata
        .get("assigned_agent")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        if assigned_agent != agent {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "wrong_agent",
                    "message": "task is assigned to another agent",
                    "assigned_agent": assigned_agent,
                })),
            )
                .into_response();
        }
    }

    let workflow_role = metadata
        .get("workflow_role")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| default_workflow_role(&task_type))
        .to_string();
    if workflow_role == "commit" {
        if let Some(finisher) = metadata
            .get("finisher_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if finisher != agent {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": "wrong_finisher",
                        "message": "task may only be claimed by the selected finisher",
                        "finisher_agent": finisher,
                    })),
                )
                    .into_response();
            }
        } else {
            metadata["finisher_agent"] = json!(agent);
        }
    }

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
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "agent_at_capacity",
                "active": active,
                "max": max_tasks,
            })),
        )
            .into_response();
    }

    // Check dependency blocking: all blocked_by tasks must be completed+approved
    let blocked_by_str: String = db
        .query_row(
            "SELECT blocked_by FROM fleet_tasks WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "[]".to_string());
    let blocked_by: Vec<String> = serde_json::from_str(&blocked_by_str).unwrap_or_default();

    for blocker_id in &blocked_by {
        if blocker_id.is_empty() {
            continue;
        }
        let satisfied: bool = db
            .query_row(
                "SELECT COUNT(*) FROM fleet_tasks WHERE id=?1 AND status='completed' \
             AND (review_result IS NULL OR review_result != 'rejected')",
                params![blocker_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        if !satisfied {
            return (
                StatusCode::LOCKED,
                Json(json!({"error":"blocked","pending":blocker_id})),
            )
                .into_response();
        }
    }

    // Atomic claim: only succeeds if still open
    if metadata
        .get("assigned_agent")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .is_none()
    {
        metadata["assigned_agent"] = json!(agent);
    }

    let rows = db.execute(
        "UPDATE fleet_tasks SET status='claimed', claimed_by=?1, claimed_at=?2, claim_expires_at=?3, updated_at=?2, metadata=?5 WHERE id=?4 AND status='open'",
        params![agent, now_str, expires_str, id, metadata.to_string()],
    ).unwrap_or(0);

    if rows == 0 {
        let exists: bool = db
            .query_row(
                "SELECT COUNT(*) FROM fleet_tasks WHERE id=?1",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;
        return if exists {
            (
                StatusCode::CONFLICT,
                Json(json!({"error":"already_claimed"})),
            )
                .into_response()
        } else {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Task not found"})),
            )
                .into_response()
        };
    }

    let task = db
        .query_row(
            &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
            params![id],
            row_to_task,
        )
        .unwrap_or(json!({"id":id}));

    let _ = state
        .bus_tx
        .send(json!({"type":"tasks:claimed","task_id":id,"agent":agent}).to_string());
    (StatusCode::OK, Json(json!({"ok":true,"task":task}))).into_response()
}

async fn unclaim_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let agent = body
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    let metadata = db
        .query_row(
            "SELECT metadata FROM fleet_tasks WHERE id=?1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .map(|mut meta| {
            if let Some(obj) = meta.as_object_mut() {
                obj.remove("assigned_agent");
                obj.remove("assigned_session");
            }
            meta.to_string()
        })
        .unwrap_or_else(|| "{}".to_string());
    let rows = db.execute(
        "UPDATE fleet_tasks SET status='open', claimed_by=NULL, claimed_at=NULL, claim_expires_at=NULL, metadata=?1, updated_at=?2 WHERE id=?3 AND (claimed_by=?4 OR ?4='')",
        params![metadata, now, id, agent],
    ).unwrap_or(0);
    if rows == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Task not found or not owned by agent"})),
        )
            .into_response();
    }
    let _ = state
        .bus_tx
        .send(json!({"type":"tasks:unclaimed","task_id":id,"agent":agent}).to_string());
    Json(json!({"ok":true})).into_response()
}

async fn complete_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let agent = body
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let output_str: Option<String> = body.get("output").map(|v| v.to_string());
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    let rows = db
        .execute(
            "UPDATE fleet_tasks SET status='completed', completed_at=?1, completed_by=?2, \
         claim_expires_at=NULL, output=?4, updated_at=?1 \
         WHERE id=?3 AND status IN ('claimed','in_progress','open')",
            params![now, agent, id, output_str],
        )
        .unwrap_or(0);
    if rows == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Task not found"})),
        )
            .into_response();
    }
    let _ = crate::routes::chains::mark_task_status(&db, &id, "completed");
    let task = db
        .query_row(
            &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
            params![id],
            row_to_task,
        )
        .unwrap_or(json!({"id":id}));
    drop(db); // release lock before potentially calling into projects state
    let _ = state
        .bus_tx
        .send(json!({"type":"tasks:completed","task_id":id,"agent":agent}).to_string());

    // Scheduler: find tasks that are now unblocked by this completion and nudge agents.
    let newly_unblocked: Vec<String> = {
        let conn = state.fleet_db.lock().await;
        crate::db::db_find_newly_unblocked(&conn, &id)
    };
    if !newly_unblocked.is_empty() {
        tracing::info!(
            component = "scheduler",
            "task {id} completed: unblocked {} dependent task(s)",
            newly_unblocked.len()
        );
        for unblocked_id in &newly_unblocked {
            let _ = state.bus_tx.send(
                json!({"type":"tasks:dispatch_nudge","task_id":unblocked_id,"reason":"blocker_completed"}).to_string()
            );
            // Collect blocked_by list for this unblocked task, then populate inputs
            let blocked_by_str: String = {
                let conn = state.fleet_db.lock().await;
                conn.query_row(
                    "SELECT blocked_by FROM fleet_tasks WHERE id=?1",
                    rusqlite::params![unblocked_id],
                    |r| r.get(0),
                )
                .unwrap_or_else(|_| "[]".to_string())
            };
            let blocked_by: Vec<String> = serde_json::from_str(&blocked_by_str).unwrap_or_default();
            {
                let conn = state.fleet_db.lock().await;
                let _ = crate::db::db_populate_inputs(&conn, unblocked_id, &blocked_by);
            }
        }
    }

    maybe_file_commit_for_outcome(&state, &task).await;

    // CCC-tk0 / drift-fix #3: any completed task may have modified the
    // project's AgentFS workspace. Mark the project dirty ONLY if the
    // workspace actually has uncommitted changes — agents whose writes
    // silently fail (e.g., bullwinkle TCC) shouldn't trigger a phase
    // commit of an unchanged tree. The milestone-commit task itself
    // calls POST /api/projects/:id/clean so it doesn't re-mark its own
    // project dirty.
    if let Some(pid) = task
        .get("project_id")
        .and_then(|v| v.as_str())
        .map(String::from)
    {
        let task_type = task
            .get("task_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !pid.is_empty() && task_type != "phase_commit" {
            let state_clone = state.clone();
            tokio::spawn(async move {
                let projects = state_clone.projects.read().await;
                let agentfs_path = projects
                    .iter()
                    .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(pid.as_str()))
                    .and_then(|p| p.get("agentfs_path").and_then(|v| v.as_str()))
                    .map(String::from);
                drop(projects);
                let actually_dirty = match agentfs_path.as_deref() {
                    Some(p) if std::path::Path::new(p).join(".git").exists() => {
                        match tokio::process::Command::new("git")
                            .args(["-C", p, "status", "--porcelain"])
                            .output()
                            .await
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
                    let _ =
                        crate::routes::projects::set_agentfs_dirty(&state_clone, &pid, true).await;
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
            meta.get("github_repo")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        ) {
            let task_id_clone = id.clone();
            let agent_clone = agent.clone();
            let auto_close = std::env::var("GITHUB_AUTO_CLOSE").unwrap_or_default() == "true";
            tokio::spawn(async move {
                notify_github_issue(
                    &gh_repo,
                    gh_num as u64,
                    &task_id_clone,
                    &agent_clone,
                    auto_close,
                )
                .await;
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let result = match body.get("result").and_then(|v| v.as_str()) {
        Some(r) if r == "approved" || r == "rejected" => r.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"result must be 'approved' or 'rejected'"})),
            )
                .into_response()
        }
    };
    let agent = body
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let notes = body
        .get("notes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();

    // Merge notes into existing metadata
    let current_meta: String = db
        .query_row(
            "SELECT metadata FROM fleet_tasks WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "{}".to_string());
    let mut meta: Value = serde_json::from_str(&current_meta).unwrap_or(json!({}));
    if !notes.is_empty() {
        meta["review_notes"] = Value::String(notes);
    }

    let rows = db
        .execute(
            "UPDATE fleet_tasks SET review_result=?1, metadata=?2, updated_at=?3 WHERE id=?4",
            params![result, meta.to_string(), now, id],
        )
        .unwrap_or(0);

    if rows == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Task not found"})),
        )
            .into_response();
    }
    let _ = crate::routes::chains::mark_task_status(
        &db,
        &id,
        if result == "rejected" {
            "rejected"
        } else {
            "review_approved"
        },
    );

    drop(db);
    let event_type = if result == "approved" {
        "tasks:review_approved"
    } else {
        "tasks:review_rejected"
    };
    let _ = state
        .bus_tx
        .send(json!({"type":event_type,"task_id":id,"agent":agent}).to_string());

    // An approval may satisfy a dependency (e.g. a blocker that was completed but
    // previously rejected). Nudge any tasks that are now fully unblocked.
    if result == "approved" {
        let newly_unblocked: Vec<String> = {
            let conn = state.fleet_db.lock().await;
            crate::db::db_find_newly_unblocked(&conn, &id)
        };
        for unblocked_id in &newly_unblocked {
            let _ = state.bus_tx.send(
                json!({"type":"tasks:dispatch_nudge","task_id":unblocked_id,"reason":"review_approved"}).to_string()
            );
        }
        maybe_file_commit_for_task_id(&state, &id).await;
    }

    Json(json!({"ok":true})).into_response()
}

async fn vote_on_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }

    let agent = match body
        .get("agent")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(a) => a.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"agent required"})),
            )
                .into_response()
        }
    };
    let vote = match body.get("vote").and_then(|v| v.as_str()) {
        Some(v) if v == "approve" || v == "reject" => v.to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"vote must be 'approve' or 'reject'"})),
            )
                .into_response()
        }
    };
    let refinement = match body.get("refinement").and_then(|v| v.as_str()) {
        Some(r) if !r.trim().is_empty() => r.trim().to_string(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"refinement required"})),
            )
                .into_response()
        }
    };

    let db = state.fleet_db.lock().await;

    // Fetch task
    let (task_type, current_meta): (String, String) = match db.query_row(
        "SELECT task_type, metadata FROM fleet_tasks WHERE id=?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(row) => row,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Task not found"})),
            )
                .into_response()
        }
    };

    if task_type != "idea" {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"task is not an idea"})),
        )
            .into_response();
    }

    let mut meta: Value = serde_json::from_str(&current_meta).unwrap_or(json!({}));

    // Prevent self-voting
    let creator = meta["created_by"].as_str().unwrap_or("").to_string();
    if !creator.is_empty() && creator == agent {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"cannot vote on own idea"})),
        )
            .into_response();
    }

    // Ensure votes array exists
    if !meta["votes"].is_array() {
        meta["votes"] = json!([]);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let votes = meta["votes"].as_array_mut().expect("votes is array");
    if let Some(existing) = votes
        .iter_mut()
        .find(|v| v["agent"].as_str() == Some(&agent))
    {
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

    let rows = db
        .execute(
            "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
            params![meta.to_string(), now, id],
        )
        .unwrap_or(0);

    if rows == 0 {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"update failed"})),
        )
            .into_response();
    }

    let task = db
        .query_row(
            &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
            params![id],
            row_to_task,
        )
        .unwrap_or(json!({"id": id}));

    let _ = state.bus_tx.send(
        json!({
            "type": "tasks:voted",
            "task_id": id,
            "agent": agent,
            "vote": vote,
        })
        .to_string(),
    );

    Json(json!({"ok":true,"task":task})).into_response()
}

async fn notify_github_issue(
    repo: &str,
    number: u64,
    task_id: &str,
    agent: &str,
    auto_close: bool,
) {
    let comment = format!(
        "✅ Fleet task `{}` completed by agent `{}`.\n\nThis issue has been resolved by the ACC agent fleet.",
        task_id, agent
    );
    let comment_status = tokio::process::Command::new("gh")
        .args([
            "issue",
            "comment",
            &number.to_string(),
            "--repo",
            repo,
            "--body",
            &comment,
        ])
        .output()
        .await;
    match comment_status {
        Ok(out) if out.status.success() => {
            tracing::info!("github: commented on {}#{}", repo, number)
        }
        Ok(out) => tracing::warn!(
            "github: comment on {}#{} failed: {}",
            repo,
            number,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => tracing::warn!("github: gh CLI not available: {}", e),
    }
    if auto_close {
        let close_status = tokio::process::Command::new("gh")
            .args(["issue", "close", &number.to_string(), "--repo", repo])
            .output()
            .await;
        match close_status {
            Ok(out) if out.status.success() => tracing::info!("github: closed {}#{}", repo, number),
            Ok(out) => tracing::warn!(
                "github: close {}#{} failed: {}",
                repo,
                number,
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => tracing::warn!("github: gh CLI close failed: {}", e),
        }
    }
}

// ── GET /api/tasks/graph ──────────────────────────────────────────────────────

async fn get_task_graph(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let db = state.fleet_db.lock().await;
    let mut stmt = match db.prepare(
        "SELECT id, title, status, task_type, blocked_by, project_id, priority \
         FROM fleet_tasks ORDER BY created_at",
    ) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":e.to_string()})),
            )
                .into_response()
        }
    };
    let mut nodes: Vec<Value> = Vec::new();
    let mut edges: Vec<Value> = Vec::new();
    let _ = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4).unwrap_or_else(|_| "[]".to_string()),
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map(|rows| {
            for (id, title, status, task_type, blocked_by_json, project_id, priority) in
                rows.flatten()
            {
                let blocked_by: Vec<String> =
                    serde_json::from_str(&blocked_by_json).unwrap_or_default();
                for blocker_id in &blocked_by {
                    edges.push(json!({"from": id, "to": blocker_id, "type": "depends_on"}));
                }
                nodes.push(json!({
                    "id": id,
                    "title": title,
                    "status": status,
                    "task_type": task_type,
                    "project_id": project_id,
                    "priority": priority,
                    "blocked_by": blocked_by
                }));
            }
        });
    Json(json!({"nodes": nodes, "edges": edges, "node_count": nodes.len(), "edge_count": edges.len()})).into_response()
}

// ── POST /api/tasks/:id/fanout ────────────────────────────────────────────────
//
// Expands a task into N parallel child tasks and transforms the original task
// into a join gate that is blocked by all children. When all children complete,
// the scheduler auto-unblocks the join task and agents can claim it again.
//
// Body: {"tasks": [{"title":"...","description":"...","task_type":"work","priority":2,"metadata":{}}]}

async fn fanout_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }

    let fanout_defs = match body.get("tasks").and_then(|v| v.as_array()) {
        Some(tasks) if !tasks.is_empty() => tasks.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"tasks array required"})),
            )
                .into_response()
        }
    };

    let now = chrono::Utc::now().to_rfc3339();

    // Read parent task — specifically its project_id and outcome identity.
    let (project_id, mut parent_metadata, parent_outcome_id): (String, Value, String) = {
        let db = state.fleet_db.lock().await;
        match db.query_row(
            "SELECT project_id, task_type, metadata FROM fleet_tasks WHERE id=?1",
            params![id],
            |row| {
                let project_id: String = row.get(0)?;
                let task_type: String = row.get(1)?;
                let metadata_str: String = row.get(2)?;
                let mut metadata: Value = serde_json::from_str(&metadata_str).unwrap_or(json!({}));
                normalize_workflow_metadata(&mut metadata, &id, &task_type);
                let outcome_id = metadata
                    .get("outcome_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(id.as_str())
                    .to_string();
                Ok((project_id, metadata, outcome_id))
            },
        ) {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error":"Task not found"})),
                )
                    .into_response()
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":e.to_string()})),
                )
                    .into_response()
            }
        }
    };

    // Create child tasks and transform the parent into a join gate.
    let mut child_ids: Vec<String> = Vec::new();
    {
        let db = state.fleet_db.lock().await;
        for task_def in &fanout_defs {
            let child_id = format!("task-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
            let title = task_def
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)");
            let desc = task_def
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let task_type = task_def
                .get("task_type")
                .and_then(|v| v.as_str())
                .unwrap_or("work");
            let priority = task_def
                .get("priority")
                .and_then(|v| v.as_i64())
                .unwrap_or(2);
            let metadata = {
                let mut metadata = task_def
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if metadata
                    .get("outcome_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .is_none()
                {
                    metadata["outcome_id"] = json!(parent_outcome_id);
                }
                if metadata
                    .get("workflow_role")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .is_none()
                {
                    metadata["workflow_role"] = json!(default_workflow_role(task_type));
                }
                metadata.to_string()
            };
            let phase = task_def
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("build");

            if db.execute(
                "INSERT INTO fleet_tasks \
                 (id, project_id, title, description, status, priority, task_type, metadata, phase, blocked_by, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6, ?7, ?8, '[]', ?9, ?9)",
                params![child_id, project_id, title, desc, priority, task_type, metadata, phase, now]
            ).is_ok() {
                child_ids.push(child_id);
            }
        }

        if child_ids.is_empty() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"failed to create child tasks"})),
            )
                .into_response();
        }

        // Transform the parent into a join gate blocked by all children.
        parent_metadata["outcome_id"] = json!(parent_outcome_id);
        parent_metadata["workflow_role"] = json!("join");
        parent_metadata["fanout_children"] = json!(child_ids);
        let blocked_by = serde_json::to_string(&child_ids).unwrap_or_else(|_| "[]".to_string());
        let _ = db.execute(
            "UPDATE fleet_tasks SET status='open', claimed_by=NULL, claimed_at=NULL, \
             claim_expires_at=NULL, blocked_by=?1, metadata=?2, updated_at=?3 WHERE id=?4",
            params![blocked_by, parent_metadata.to_string(), now, id],
        );
    }

    // Announce new children and nudge agents to pick them up.
    let _ = state.bus_tx.send(
        json!({"type":"tasks:fanout","parent_id":id,"children":child_ids,"count":child_ids.len()})
            .to_string(),
    );
    for child_id in &child_ids {
        let _ = state.bus_tx.send(
            json!({"type":"tasks:added","task_id":child_id,"project_id":project_id}).to_string(),
        );
        let _ = state.bus_tx.send(
            json!({"type":"tasks:dispatch_nudge","task_id":child_id,"reason":"fanout"}).to_string(),
        );
    }

    (
        StatusCode::CREATED,
        Json(json!({"ok":true,"parent_id":id,"children":child_ids})),
    )
        .into_response()
}

async fn maybe_file_commit_for_task_id(state: &Arc<AppState>, task_id: &str) {
    let task = {
        let db = state.fleet_db.lock().await;
        db.query_row(
            &format!("SELECT {TASK_COLS} FROM fleet_tasks WHERE id=?1"),
            params![task_id],
            row_to_task,
        )
        .ok()
    };
    if let Some(task) = task {
        maybe_file_commit_for_outcome(state, &task).await;
    }
}

async fn maybe_file_commit_for_outcome(state: &Arc<AppState>, trigger_task: &Value) {
    let outcome_id = match task_outcome_id(trigger_task) {
        Some(id) => id,
        None => return,
    };
    let project_id = match trigger_task.get("project_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return,
    };

    let outcome_tasks = fetch_project_outcome_tasks(state, &project_id, &outcome_id).await;
    if outcome_tasks.is_empty() {
        return;
    }
    if outcome_tasks
        .iter()
        .any(|t| task_workflow_role(t) == "commit")
    {
        return;
    }

    let joins: Vec<&Value> = outcome_tasks
        .iter()
        .filter(|t| task_workflow_role(t) == "join")
        .collect();
    let completed_join = joins
        .iter()
        .find(|t| t.get("status").and_then(|v| v.as_str()) == Some("completed"));
    if !joins.is_empty() && completed_join.is_none() {
        return;
    }
    if joins.is_empty() {
        // Migration safety: do not invent finalization for older ad-hoc
        // outcomes until an explicit join gate exists.
        return;
    }

    let work_tasks: Vec<&Value> = outcome_tasks
        .iter()
        .filter(|t| task_workflow_role(t) == "work")
        .collect();
    if work_tasks.is_empty() {
        return;
    }
    if work_tasks.iter().any(|t| {
        t.get("status").and_then(|v| v.as_str()) != Some("completed")
            || t.get("review_result").and_then(|v| v.as_str()) != Some("approved")
    }) {
        return;
    }

    let gaps: Vec<&Value> = outcome_tasks
        .iter()
        .filter(|t| task_workflow_role(t) == "gap")
        .collect();
    if gaps.iter().any(|t| {
        t.get("status").and_then(|v| v.as_str()) != Some("completed")
            || t.get("review_result").and_then(|v| v.as_str()) == Some("rejected")
    }) {
        return;
    }

    let agents = state.agents.read().await.clone();
    let (finisher_agent, finisher_session) =
        match select_finisher_for_outcome(&outcome_tasks, &agents, &project_id) {
            Some(f) => f,
            None => return,
        };
    persist_finisher_for_outcome(
        state,
        &project_id,
        &outcome_id,
        &finisher_agent,
        finisher_session.as_deref(),
    )
    .await;

    let phase = outcome_tasks
        .iter()
        .filter_map(|t| t.get("phase").and_then(|v| v.as_str()))
        .find(|s| !s.is_empty())
        .unwrap_or("milestone")
        .to_string();
    let join_id = completed_join
        .and_then(|t| t.get("id").and_then(|v| v.as_str()))
        .unwrap_or("");
    let blockers = if join_id.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(&vec![join_id]).unwrap_or_else(|_| "[]".to_string())
    };
    let task_id = format!("task-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let title = format!("Commit outcome {outcome_id}");
    let description = format!(
        "Workflow-ready commit task for outcome {outcome_id}. \
         All required work has completed, reviews are approved, and the join gate is satisfied."
    );
    let mut metadata = json!({
        "source": "workflow-ready-commit",
        "outcome_id": outcome_id,
        "workflow_role": "commit",
        "finisher_agent": finisher_agent,
    });
    if let Some(session) = finisher_session.as_deref() {
        metadata["finisher_session"] = json!(session);
    }

    let inserted = {
        let db = state.fleet_db.lock().await;
        db.execute(
            "INSERT INTO fleet_tasks
              (id, project_id, title, description, priority, metadata, task_type, phase, blocked_by)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, 'phase_commit', ?6, ?7)",
            params![
                task_id,
                project_id,
                title,
                description,
                metadata.to_string(),
                phase,
                blockers
            ],
        )
        .unwrap_or(0)
    };
    if inserted > 0 {
        let _ = state.bus_tx.send(
            json!({
                "type": "tasks:added",
                "task_id": task_id,
                "task_type": "phase_commit",
                "project_id": project_id,
                "outcome_id": metadata["outcome_id"],
                "workflow_role": "commit",
                "finisher_agent": metadata["finisher_agent"],
            })
            .to_string(),
        );
        let _ = state.bus_tx.send(
            json!({
                "type": "tasks:dispatch_nudge",
                "task_id": task_id,
                "reason": "outcome_ready",
            })
            .to_string(),
        );
    }
}

async fn fetch_project_outcome_tasks(
    state: &Arc<AppState>,
    project_id: &str,
    outcome_id: &str,
) -> Vec<Value> {
    let db = state.fleet_db.lock().await;
    let mut stmt = match db.prepare(&format!(
        "SELECT {TASK_COLS} FROM fleet_tasks WHERE project_id=?1 ORDER BY created_at ASC"
    )) {
        Ok(stmt) => stmt,
        Err(_) => return vec![],
    };
    let rows = match stmt.query_map(params![project_id], row_to_task) {
        Ok(rows) => rows,
        Err(_) => return vec![],
    };
    rows.filter_map(|r| r.ok())
        .filter(|task| task_outcome_id(task).as_deref() == Some(outcome_id))
        .collect()
}

fn task_outcome_id(task: &Value) -> Option<String> {
    task.get("outcome_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            task.get("metadata")
                .and_then(|m| m.get("outcome_id"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            task.get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
}

fn task_workflow_role(task: &Value) -> String {
    task.get("workflow_role")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            task.get("metadata")
                .and_then(|m| m.get("workflow_role"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
        .unwrap_or_else(|| {
            default_workflow_role(
                task.get("task_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("work"),
            )
            .to_string()
        })
}

fn select_finisher_for_outcome(
    outcome_tasks: &[Value],
    agents: &Value,
    project_id: &str,
) -> Option<(String, Option<String>)> {
    if let Some(existing) = outcome_tasks
        .iter()
        .filter_map(|task| {
            task.get("finisher_agent")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    task.get("metadata")
                        .and_then(|m| m.get("finisher_agent"))
                        .and_then(|v| v.as_str())
                })
        })
        .find(|s| !s.is_empty())
    {
        let session = outcome_tasks
            .iter()
            .filter_map(|task| {
                task.get("finisher_session")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        task.get("metadata")
                            .and_then(|m| m.get("finisher_session"))
                            .and_then(|v| v.as_str())
                    })
            })
            .find(|s| !s.is_empty())
            .map(str::to_string);
        return Some((existing.to_string(), session));
    }

    if let Some((agent, session)) = select_project_session_finisher(agents, project_id) {
        return Some((agent, Some(session)));
    }

    let mut completed_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for task in outcome_tasks
        .iter()
        .filter(|t| task_workflow_role(t) == "work")
    {
        if let Some(agent) = task
            .get("completed_by")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            *completed_counts.entry(agent.to_string()).or_insert(0) += 1;
        }
    }
    let mut counted: Vec<(String, usize, bool)> = completed_counts
        .into_iter()
        .map(|(name, count)| {
            let online = agents
                .get(&name)
                .map(crate::dispatch::is_agent_online)
                .unwrap_or(false);
            (name, count, online)
        })
        .collect();
    counted.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    if let Some((agent, _, _)) = counted.into_iter().next() {
        return Some((agent, None));
    }

    let synthetic = json!({
        "id": "finisher-selection",
        "project_id": project_id,
        "metadata": {},
    });
    crate::dispatch::select_best_agent(&synthetic, agents, &Default::default(), &[], usize::MAX)
        .map(|agent| (agent, None))
}

fn select_project_session_finisher(agents: &Value, project_id: &str) -> Option<(String, String)> {
    let map = agents.as_object()?;
    let mut candidates: Vec<(String, String)> = Vec::new();
    for (name, agent) in map {
        if !crate::dispatch::is_agent_online(agent) {
            continue;
        }
        let sessions = agent
            .get("sessions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for session in sessions {
            if session.get("project_id").and_then(|v| v.as_str()) != Some(project_id) {
                continue;
            }
            if !session_is_healthy(&session) {
                continue;
            }
            if let Some(session_name) = session
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                candidates.push((name.clone(), session_name.to_string()));
            }
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

fn session_is_healthy(session: &Value) -> bool {
    let state = session
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("idle");
    let auth = session
        .get("auth_state")
        .and_then(|v| v.as_str())
        .unwrap_or("ready");
    !matches!(state, "dead" | "unauthenticated" | "stuck") && auth != "unauthenticated"
}

async fn persist_finisher_for_outcome(
    state: &Arc<AppState>,
    project_id: &str,
    outcome_id: &str,
    finisher_agent: &str,
    finisher_session: Option<&str>,
) {
    let db = state.fleet_db.lock().await;
    let mut stmt =
        match db.prepare("SELECT id, task_type, metadata FROM fleet_tasks WHERE project_id=?1") {
            Ok(stmt) => stmt,
            Err(_) => return,
        };
    let rows = match stmt.query_map(params![project_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(_) => return,
    };
    let tasks: Vec<(String, String, String)> = rows.filter_map(|r| r.ok()).collect();
    drop(stmt);
    for (id, task_type, raw_meta) in tasks {
        let mut meta: Value = serde_json::from_str(&raw_meta).unwrap_or(json!({}));
        normalize_workflow_metadata(&mut meta, &id, &task_type);
        if meta.get("outcome_id").and_then(|v| v.as_str()) != Some(outcome_id) {
            continue;
        }
        if meta
            .get("finisher_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_none()
        {
            meta["finisher_agent"] = json!(finisher_agent);
        }
        if let Some(session) = finisher_session {
            if meta
                .get("finisher_session")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .is_none()
            {
                meta["finisher_session"] = json!(session);
            }
        }
        let _ = db.execute(
            "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
            params![meta.to_string(), chrono::Utc::now().to_rfc3339(), id],
        );
    }
}

// ── PUT /api/tasks/:id/keepalive ─────────────────────────────────────────────

async fn keepalive_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let agent = body
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let extend_mins: i64 = body
        .get("extend_mins")
        .and_then(|v| v.as_i64())
        .unwrap_or(30);
    let now = chrono::Utc::now();
    let new_expires = (now + chrono::Duration::minutes(extend_mins)).to_rfc3339();
    let db = state.fleet_db.lock().await;
    let rows = db
        .execute(
            "UPDATE fleet_tasks SET claim_expires_at=?1, updated_at=?2 \
         WHERE id=?3 AND claimed_by=?4 AND status IN ('claimed','in_progress')",
            params![new_expires, now.to_rfc3339(), id, agent],
        )
        .unwrap_or(0);
    if rows == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"task not found or not claimed by this agent"})),
        )
            .into_response();
    }
    Json(json!({"ok":true,"claim_expires_at":new_expires})).into_response()
}

// ── POST /api/tasks/:id/turns ─────────────────────────────────────────────────

async fn append_turn(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let turn_index = body["turn_index"].as_i64().unwrap_or(0);
    let role = body["role"].as_str().unwrap_or("assistant").to_string();
    let content = body
        .get("content")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "[]".to_string());
    let input_tokens = body["input_tokens"].as_i64().unwrap_or(0);
    let output_tokens = body["output_tokens"].as_i64().unwrap_or(0);
    let stop_reason = body["stop_reason"].as_str().map(str::to_string);
    let db = state.fleet_db.lock().await;
    match crate::db::db_save_turn(
        &db,
        &id,
        turn_index,
        &role,
        &content,
        input_tokens,
        output_tokens,
        stop_reason.as_deref(),
    ) {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/tasks/:id/turns ──────────────────────────────────────────────────

async fn get_turns(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let db = state.fleet_db.lock().await;
    let turns = crate::db::db_load_turns(&db, &id);
    Json(json!({"ok":true,"turns":turns,"count":turns.len()})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{self, body_json, post_json, TestServer};
    use axum::body::Body;
    use axum::http::Request;

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
        let resp = testing::call(
            &server.app,
            post_json(
                "/api/tasks",
                &json!({
                    "project_id": project,
                    "title": title,
                    "task_type": "idea",
                    "agent": creator,
                }),
            ),
        )
        .await;
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

        let resp = testing::call(
            &server.app,
            put_json(
                &format!("/api/tasks/{}/vote", task_id),
                &json!({"agent":"bob","vote":"approve"}),
            ),
        )
        .await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_vote_empty_refinement_rejected() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        let resp = testing::call(
            &server.app,
            put_json(
                &format!("/api/tasks/{}/vote", task_id),
                &json!({"agent":"bob","vote":"approve","refinement":"   "}),
            ),
        )
        .await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_vote_self_vote_rejected() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        let resp = testing::call(
            &server.app,
            put_json(
                &format!("/api/tasks/{}/vote", task_id),
                &json!({"agent":"alice","vote":"approve","refinement":"I like my own idea"}),
            ),
        )
        .await;
        assert_eq!(resp.status(), 409);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("own idea"));
    }

    #[tokio::test]
    async fn test_vote_on_non_idea_rejected() {
        let server = TestServer::new().await;
        let resp = testing::call(
            &server.app,
            post_json(
                "/api/tasks",
                &json!({
                    "project_id": "proj-a",
                    "title": "A work task",
                    "task_type": "work",
                }),
            ),
        )
        .await;
        let body = body_json(resp).await;
        let task_id = body["task"]["id"].as_str().unwrap();

        let resp = testing::call(
            &server.app,
            put_json(
                &format!("/api/tasks/{}/vote", task_id),
                &json!({"agent":"bob","vote":"approve","refinement":"looks good"}),
            ),
        )
        .await;
        assert_eq!(resp.status(), 409);
    }

    #[tokio::test]
    async fn test_vote_updates_existing_vote() {
        let server = TestServer::new().await;
        let task = create_idea(&server, "proj-a", "My idea", "alice").await;
        let task_id = task["id"].as_str().unwrap();

        // First vote
        testing::call(
            &server.app,
            put_json(
                &format!("/api/tasks/{}/vote", task_id),
                &json!({"agent":"bob","vote":"reject","refinement":"Not sure about this"}),
            ),
        )
        .await;

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

    #[tokio::test]
    async fn test_list_tasks_filter_by_source() {
        let server = TestServer::new().await;
        // Create a fleet task with default source
        let body = json!({
            "project_id": "proj-src", "title": "Fleet task", "description": "test",
            "priority": 2, "task_type": "work"
        });
        let resp = testing::call(&server.app, post_json("/api/tasks", &body)).await;
        assert_eq!(resp.status(), StatusCode::CREATED);

        // List with source=fleet should return it
        let resp = testing::call(&server.app, testing::get("/api/tasks?source=fleet")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let data = testing::body_json(resp).await;
        assert!(data["count"].as_u64().unwrap_or(0) >= 1);

        // List with source=queue should return empty (no queue-sourced tasks created)
        let resp = testing::call(&server.app, testing::get("/api/tasks?source=queue")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let data = testing::body_json(resp).await;
        assert_eq!(data["count"].as_u64().unwrap_or(0), 0);
    }
}
