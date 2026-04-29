//! /api/chains — durable cross-platform conversation chains.
//!
//! A chain is the shared provenance object for Slack threads, Telegram chats,
//! reactions, bot replies, task links, and final outcome signals. Raw events are
//! append-only; the chain row and participant/entity/task tables are derived
//! indexes for fast queries.

use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/chains", get(list_chains).post(upsert_chain))
        .route("/api/chains/:id", get(get_chain).patch(patch_chain))
        .route("/api/chains/:id/events", post(append_event))
        .route("/api/chains/:id/tasks", post(link_task))
}

#[derive(Deserialize)]
struct ChainQuery {
    source: Option<String>,
    workspace: Option<String>,
    channel_id: Option<String>,
    status: Option<String>,
    participant: Option<String>,
    entity_type: Option<String>,
    entity_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

fn authed(state: &AppState, headers: &HeaderMap) -> Option<axum::response::Response> {
    if state.is_authed(headers) {
        None
    } else {
        Some(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error":"Unauthorized"})),
            )
                .into_response(),
        )
    }
}

async fn list_chains(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ChainQuery>,
) -> impl IntoResponse {
    if let Some(resp) = authed(&state, &headers) {
        return resp;
    }

    let db = state.fleet_db.lock().await;
    let mut sql = "SELECT id,source,workspace,channel_id,thread_id,root_event_id,title,summary,status,outcome,created_at,updated_at,closed_at,metadata FROM conversation_chains WHERE 1=1".to_string();
    let mut binds: Vec<String> = Vec::new();
    if let Some(v) = q.source {
        sql.push_str(" AND source=?");
        binds.push(v);
    }
    if let Some(v) = q.workspace {
        sql.push_str(" AND workspace=?");
        binds.push(v);
    }
    if let Some(v) = q.channel_id {
        sql.push_str(" AND channel_id=?");
        binds.push(v);
    }
    if let Some(v) = q.status {
        sql.push_str(" AND status=?");
        binds.push(v);
    }
    if let Some(v) = q.participant {
        sql.push_str(" AND id IN (SELECT chain_id FROM conversation_chain_participants WHERE participant_id=?)");
        binds.push(v);
    }
    if let (Some(t), Some(id)) = (q.entity_type, q.entity_id) {
        sql.push_str(" AND id IN (SELECT chain_id FROM conversation_chain_entities WHERE entity_type=? AND entity_id=?)");
        binds.push(t);
        binds.push(id);
    }
    sql.push_str(" ORDER BY updated_at DESC");
    let limit = q.limit.unwrap_or(100).min(500);
    let offset = q.offset.unwrap_or(0);
    sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

    let mut stmt = match db.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => return server_error(e).into_response(),
    };
    let chains: Vec<Value> = stmt
        .query_map(
            rusqlite::params_from_iter(binds.iter().map(|s| s.as_str())),
            row_to_chain,
        )
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    Json(json!({"chains": chains, "count": chains.len()})).into_response()
}

async fn upsert_chain(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(resp) = authed(&state, &headers) {
        return resp;
    }

    let db = state.fleet_db.lock().await;
    match upsert_chain_from_body(&db, &body) {
        Ok(id) => match load_chain_full(&db, &id) {
            Ok(Some(chain)) => (
                StatusCode::CREATED,
                Json(json!({"ok": true, "chain": chain})),
            )
                .into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"chain not found"})),
            )
                .into_response(),
            Err(e) => server_error(e).into_response(),
        },
        Err(e) => server_error(e).into_response(),
    }
}

async fn get_chain(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(resp) = authed(&state, &headers) {
        return resp;
    }

    let db = state.fleet_db.lock().await;
    match load_chain_full(&db, &id) {
        Ok(Some(chain)) => Json(chain).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"chain not found"})),
        )
            .into_response(),
        Err(e) => server_error(e).into_response(),
    }
}

async fn patch_chain(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(resp) = authed(&state, &headers) {
        return resp;
    }

    let db = state.fleet_db.lock().await;
    if !chain_exists(&db, &id).unwrap_or(false) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"chain not found"})),
        )
            .into_response();
    }

    let now = now_ts();
    let title = string_field(&body, &["title"]);
    let summary = string_field(&body, &["summary"]);
    let status = string_field(&body, &["status"]);
    let outcome = string_field(&body, &["outcome"]);
    let metadata = body.get("metadata").map(json_string);
    let terminal = status.as_deref().map(is_terminal_status).unwrap_or(false);
    let closed_at = body
        .get("closed_at")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| terminal.then(|| now.clone()));

    let result = db.execute(
        "UPDATE conversation_chains SET
             title=COALESCE(?1,title),
             summary=COALESCE(?2,summary),
             status=COALESCE(?3,status),
             outcome=COALESCE(?4,outcome),
             metadata=COALESCE(?5,metadata),
             closed_at=COALESCE(?6,closed_at),
             updated_at=?7
         WHERE id=?8",
        params![title, summary, status, outcome, metadata, closed_at, now, id],
    );
    match result {
        Ok(_) => match load_chain_full(&db, &id) {
            Ok(Some(chain)) => Json(json!({"ok": true, "chain": chain})).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"chain not found"})),
            )
                .into_response(),
            Err(e) => server_error(e).into_response(),
        },
        Err(e) => server_error(e).into_response(),
    }
}

async fn append_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(chain_id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(resp) = authed(&state, &headers) {
        return resp;
    }

    let event_type = match string_field(&body, &["event_type", "type"]) {
        Some(v) if !v.is_empty() => v,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"event_type required"})),
            )
                .into_response()
        }
    };

    let db = state.fleet_db.lock().await;
    if !chain_exists(&db, &chain_id).unwrap_or(false) {
        let mut chain_body = body.get("chain").cloned().unwrap_or_else(|| json!({}));
        chain_body["id"] = json!(chain_id);
        if chain_body.get("source").is_none() {
            chain_body["source"] = body
                .get("source")
                .cloned()
                .unwrap_or_else(|| json!("gateway"));
        }
        if let Err(e) = upsert_chain_from_body(&db, &chain_body) {
            return server_error(e).into_response();
        }
    }

    match append_event_inner(&db, &chain_id, &event_type, &body) {
        Ok(event) => match load_chain_full(&db, &chain_id) {
            Ok(Some(chain)) => {
                Json(json!({"ok": true, "event": event, "chain": chain})).into_response()
            }
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"chain not found"})),
            )
                .into_response(),
            Err(e) => server_error(e).into_response(),
        },
        Err(e) => server_error(e).into_response(),
    }
}

async fn link_task(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(chain_id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Some(resp) = authed(&state, &headers) {
        return resp;
    }
    let task_id = match string_field(&body, &["task_id", "taskId"]) {
        Some(v) if !v.is_empty() => v,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"task_id required"})),
            )
                .into_response()
        }
    };
    let relationship =
        string_field(&body, &["relationship"]).unwrap_or_else(|| "spawned".to_string());
    let metadata = body.get("metadata").cloned().unwrap_or_else(|| json!({}));

    let db = state.fleet_db.lock().await;
    if let Err(e) = ensure_chain_stub(&db, &chain_id) {
        return server_error(e).into_response();
    }
    match link_task_to_chain(&db, &chain_id, &task_id, &relationship, &metadata) {
        Ok(()) => match load_chain_full(&db, &chain_id) {
            Ok(Some(chain)) => Json(json!({"ok": true, "chain": chain})).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"chain not found"})),
            )
                .into_response(),
            Err(e) => server_error(e).into_response(),
        },
        Err(e) => server_error(e).into_response(),
    }
}

pub(crate) fn link_task_to_chain(
    conn: &rusqlite::Connection,
    chain_id: &str,
    task_id: &str,
    relationship: &str,
    metadata: &Value,
) -> rusqlite::Result<()> {
    ensure_chain_stub(conn, chain_id)?;
    let now = now_ts();
    conn.execute(
        "INSERT INTO conversation_chain_tasks
             (chain_id, task_id, relationship, created_at, metadata)
         VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(chain_id, task_id) DO UPDATE SET
             relationship=excluded.relationship,
             metadata=excluded.metadata",
        params![chain_id, task_id, relationship, now, json_string(metadata)],
    )?;
    upsert_entity(
        conn,
        chain_id,
        "task",
        task_id,
        Some(task_id.to_string()),
        &now,
        &json!({"relationship": relationship}),
    )?;
    conn.execute(
        "UPDATE conversation_chains SET updated_at=?1 WHERE id=?2",
        params![now, chain_id],
    )?;
    Ok(())
}

pub(crate) fn mark_task_status(
    conn: &rusqlite::Connection,
    task_id: &str,
    status: &str,
) -> rusqlite::Result<()> {
    let now = now_ts();
    let mut stmt =
        conn.prepare("SELECT chain_id, metadata FROM conversation_chain_tasks WHERE task_id=?1")?;
    let rows = stmt.query_map(params![task_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let linked: Vec<(String, String)> = rows.filter_map(|r| r.ok()).collect();
    drop(stmt);

    for (chain_id, raw_meta) in linked {
        let mut meta: Value = serde_json::from_str(&raw_meta).unwrap_or_else(|_| json!({}));
        if !meta.is_object() {
            meta = json!({});
        }
        meta["last_task_status"] = json!(status);
        meta["last_task_status_at"] = json!(now.clone());
        let resolved_at: Option<String> =
            if matches!(status, "completed" | "cancelled" | "rejected") {
                Some(now.clone())
            } else {
                None
            };
        conn.execute(
            "UPDATE conversation_chain_tasks
             SET metadata=?1,
                 resolved_at=CASE WHEN ?2 IS NOT NULL THEN COALESCE(resolved_at, ?2) ELSE resolved_at END
             WHERE chain_id=?3 AND task_id=?4",
            params![json_string(&meta), resolved_at, chain_id, task_id],
        )?;
        conn.execute(
            "UPDATE conversation_chains SET updated_at=?1 WHERE id=?2",
            params![now, chain_id],
        )?;
    }
    Ok(())
}

fn upsert_chain_from_body(conn: &rusqlite::Connection, body: &Value) -> rusqlite::Result<String> {
    let id = string_field(body, &["id"]).unwrap_or_else(|| chain_uuid("chain"));
    let source = string_field(body, &["source"]).unwrap_or_else(|| "api".to_string());
    let workspace = string_field(body, &["workspace"]).unwrap_or_default();
    let channel_id = string_field(body, &["channel_id", "channel"]).unwrap_or_default();
    let thread_id = string_field(body, &["thread_id", "thread"]).unwrap_or_default();
    let root_event_id = string_field(body, &["root_event_id", "root"]);
    let title = string_field(body, &["title"]).unwrap_or_default();
    let summary = string_field(body, &["summary"]).unwrap_or_default();
    let status_supplied = body.get("status").is_some();
    let status = string_field(body, &["status"]).unwrap_or_else(|| "active".to_string());
    let outcome = string_field(body, &["outcome"]);
    let now = now_ts();
    let closed_at = body
        .get("closed_at")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| (status_supplied && is_terminal_status(&status)).then(|| now.clone()));
    let metadata = body.get("metadata").cloned().unwrap_or_else(|| json!({}));
    let metadata_s = json_string(&metadata);
    let status_update = status_supplied.then(|| status.clone());

    conn.execute(
        "INSERT INTO conversation_chains
             (id,source,workspace,channel_id,thread_id,root_event_id,title,summary,status,outcome,created_at,updated_at,closed_at,metadata)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11,?12,?13)
         ON CONFLICT(id) DO UPDATE SET
             source=excluded.source,
             workspace=excluded.workspace,
             channel_id=excluded.channel_id,
             thread_id=excluded.thread_id,
             root_event_id=COALESCE(excluded.root_event_id, conversation_chains.root_event_id),
             title=CASE WHEN excluded.title != '' THEN excluded.title ELSE conversation_chains.title END,
             summary=CASE WHEN excluded.summary != '' THEN excluded.summary ELSE conversation_chains.summary END,
             status=COALESCE(?14, conversation_chains.status),
             outcome=COALESCE(excluded.outcome, conversation_chains.outcome),
             updated_at=excluded.updated_at,
             closed_at=COALESCE(excluded.closed_at, conversation_chains.closed_at),
             metadata=CASE WHEN excluded.metadata != '{}' THEN excluded.metadata ELSE conversation_chains.metadata END",
        params![
            id,
            source,
            workspace,
            channel_id,
            thread_id,
            root_event_id,
            title,
            summary,
            status,
            outcome,
            now,
            closed_at,
            metadata_s,
            status_update,
        ],
    )?;

    if let Some(participants) = body.get("participants").and_then(|v| v.as_array()) {
        for participant in participants {
            upsert_participant_value(conn, &id, &source, participant)?;
        }
    }
    if let Some(entities) = body.get("entities").and_then(|v| v.as_array()) {
        for entity in entities {
            upsert_entity_value(conn, &id, entity)?;
        }
    }
    Ok(id)
}

fn append_event_inner(
    conn: &rusqlite::Connection,
    chain_id: &str,
    event_type: &str,
    body: &Value,
) -> rusqlite::Result<Value> {
    let source_event_id = string_field(body, &["source_event_id", "sourceEventId"]);
    let id = string_field(body, &["id"]).unwrap_or_else(|| chain_uuid("evt"));
    let actor_id = string_field(body, &["actor_id", "actorId"])
        .or_else(|| body.get("actor").and_then(|a| string_field(a, &["id"])));
    let actor_name = string_field(body, &["actor_name", "actorName"]).or_else(|| {
        body.get("actor")
            .and_then(|a| string_field(a, &["name", "display_name"]))
    });
    let actor_kind = string_field(body, &["actor_kind", "actorKind"])
        .or_else(|| body.get("actor").and_then(|a| string_field(a, &["kind"])));
    let text = string_field(body, &["text", "body"]);
    let occurred_at = string_field(body, &["occurred_at", "ts"]).unwrap_or_else(now_ts);
    let metadata = body.get("metadata").cloned().unwrap_or_else(|| json!({}));

    conn.execute(
        "INSERT OR IGNORE INTO conversation_chain_events
             (id,chain_id,event_type,source_event_id,actor_id,actor_name,actor_kind,text,occurred_at,metadata)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            id,
            chain_id,
            event_type,
            source_event_id,
            actor_id,
            actor_name,
            actor_kind,
            text,
            occurred_at,
            json_string(&metadata),
        ],
    )?;

    let event = load_event(conn, chain_id, &id, source_event_id.as_deref())?;
    let source = string_field(body, &["source"])
        .or_else(|| chain_source(conn, chain_id).ok().flatten())
        .unwrap_or_default();
    if let Some(actor_id) = actor_id.as_deref().filter(|s| !s.is_empty()) {
        upsert_participant(
            conn,
            chain_id,
            &source,
            actor_id,
            actor_name,
            actor_kind.unwrap_or_else(|| "human".to_string()),
            &occurred_at,
            &json!({}),
        )?;
    }
    if let Some(participants) = body.get("participants").and_then(|v| v.as_array()) {
        for participant in participants {
            upsert_participant_value(conn, chain_id, &source, participant)?;
        }
    }
    if let Some(entities) = body.get("entities").and_then(|v| v.as_array()) {
        for entity in entities {
            upsert_entity_value(conn, chain_id, entity)?;
        }
    }
    if let Some(task_id) = string_field(body, &["task_id", "taskId"]) {
        link_task_to_chain(
            conn,
            chain_id,
            &task_id,
            "mentioned",
            &json!({"event_type": event_type}),
        )?;
    }

    update_chain_from_event(
        conn,
        chain_id,
        event_type,
        body,
        text.as_deref(),
        &occurred_at,
    )?;
    Ok(event)
}

fn update_chain_from_event(
    conn: &rusqlite::Connection,
    chain_id: &str,
    event_type: &str,
    body: &Value,
    text: Option<&str>,
    now: &str,
) -> rusqlite::Result<()> {
    let status = string_field(body, &["status"]).or_else(|| match event_type {
        "chain_closed" => Some("closed".to_string()),
        _ => None,
    });
    let outcome = string_field(body, &["outcome"]).or_else(|| match event_type {
        "error" | "error_seen" => Some("failure".to_string()),
        _ => None,
    });
    let closed_at = status
        .as_deref()
        .filter(|s| is_terminal_status(s))
        .map(|_| now.to_string());
    let title = text
        .filter(|s| !s.trim().is_empty())
        .map(compact_title)
        .unwrap_or_default();
    conn.execute(
        "UPDATE conversation_chains SET
             status=COALESCE(?1,status),
             outcome=COALESCE(?2,outcome),
             closed_at=COALESCE(?3,closed_at),
             title=CASE WHEN title='' AND ?4 != '' THEN ?4 ELSE title END,
             updated_at=?5
         WHERE id=?6",
        params![status, outcome, closed_at, title, now, chain_id],
    )?;
    Ok(())
}

fn load_chain_full(conn: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    let mut chain = match conn
        .query_row(
            "SELECT id,source,workspace,channel_id,thread_id,root_event_id,title,summary,status,outcome,created_at,updated_at,closed_at,metadata
             FROM conversation_chains WHERE id=?1",
            params![id],
            row_to_chain,
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };
    chain["participants"] = json!(load_participants(conn, id)?);
    chain["entities"] = json!(load_entities(conn, id)?);
    chain["tasks"] = json!(load_chain_tasks(conn, id)?);
    chain["events"] = json!(load_events(conn, id)?);
    Ok(Some(chain))
}

fn row_to_chain(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let metadata: String = row.get(13)?;
    Ok(json!({
        "id":            row.get::<_, String>(0)?,
        "source":        row.get::<_, String>(1)?,
        "workspace":     row.get::<_, String>(2)?,
        "channel_id":    row.get::<_, String>(3)?,
        "thread_id":     row.get::<_, String>(4)?,
        "root_event_id": row.get::<_, Option<String>>(5)?,
        "title":         row.get::<_, String>(6)?,
        "summary":       row.get::<_, String>(7)?,
        "status":        row.get::<_, String>(8)?,
        "outcome":       row.get::<_, Option<String>>(9)?,
        "created_at":    row.get::<_, String>(10)?,
        "updated_at":    row.get::<_, String>(11)?,
        "closed_at":     row.get::<_, Option<String>>(12)?,
        "metadata":      serde_json::from_str::<Value>(&metadata).unwrap_or_else(|_| json!({})),
    }))
}

fn load_event(
    conn: &rusqlite::Connection,
    chain_id: &str,
    event_id: &str,
    source_event_id: Option<&str>,
) -> rusqlite::Result<Value> {
    if let Some(source_event_id) = source_event_id.filter(|s| !s.is_empty()) {
        return conn.query_row(
            "SELECT id,chain_id,event_type,source_event_id,actor_id,actor_name,actor_kind,text,occurred_at,created_at,metadata
             FROM conversation_chain_events WHERE chain_id=?1 AND source_event_id=?2",
            params![chain_id, source_event_id],
            row_to_event,
        );
    }
    conn.query_row(
        "SELECT id,chain_id,event_type,source_event_id,actor_id,actor_name,actor_kind,text,occurred_at,created_at,metadata
         FROM conversation_chain_events WHERE id=?1",
        params![event_id],
        row_to_event,
    )
}

fn load_events(conn: &rusqlite::Connection, chain_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id,chain_id,event_type,source_event_id,actor_id,actor_name,actor_kind,text,occurred_at,created_at,metadata
         FROM conversation_chain_events WHERE chain_id=?1 ORDER BY occurred_at ASC, created_at ASC",
    )?;
    let rows = stmt.query_map(params![chain_id], row_to_event)?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn row_to_event(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let metadata: String = row.get(10)?;
    Ok(json!({
        "id":              row.get::<_, String>(0)?,
        "chain_id":        row.get::<_, String>(1)?,
        "event_type":      row.get::<_, String>(2)?,
        "source_event_id": row.get::<_, Option<String>>(3)?,
        "actor_id":        row.get::<_, Option<String>>(4)?,
        "actor_name":      row.get::<_, Option<String>>(5)?,
        "actor_kind":      row.get::<_, Option<String>>(6)?,
        "text":            row.get::<_, Option<String>>(7)?,
        "occurred_at":     row.get::<_, String>(8)?,
        "created_at":      row.get::<_, String>(9)?,
        "metadata":        serde_json::from_str::<Value>(&metadata).unwrap_or_else(|_| json!({})),
    }))
}

fn load_participants(conn: &rusqlite::Connection, chain_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT participant_id,platform,display_name,participant_kind,first_seen_at,last_seen_at,metadata
         FROM conversation_chain_participants WHERE chain_id=?1 ORDER BY first_seen_at ASC",
    )?;
    let rows = stmt.query_map(params![chain_id], |row| {
        let metadata: String = row.get(6)?;
        Ok(json!({
            "id":         row.get::<_, String>(0)?,
            "platform":   row.get::<_, String>(1)?,
            "name":       row.get::<_, Option<String>>(2)?,
            "kind":       row.get::<_, String>(3)?,
            "first_seen": row.get::<_, String>(4)?,
            "last_seen":  row.get::<_, String>(5)?,
            "metadata":   serde_json::from_str::<Value>(&metadata).unwrap_or_else(|_| json!({})),
        }))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn load_entities(conn: &rusqlite::Connection, chain_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT entity_type,entity_id,label,first_seen_at,last_seen_at,metadata
         FROM conversation_chain_entities WHERE chain_id=?1 ORDER BY entity_type, entity_id",
    )?;
    let rows = stmt.query_map(params![chain_id], |row| {
        let metadata: String = row.get(5)?;
        Ok(json!({
            "type":       row.get::<_, String>(0)?,
            "id":         row.get::<_, String>(1)?,
            "label":      row.get::<_, Option<String>>(2)?,
            "first_seen": row.get::<_, String>(3)?,
            "last_seen":  row.get::<_, String>(4)?,
            "metadata":   serde_json::from_str::<Value>(&metadata).unwrap_or_else(|_| json!({})),
        }))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn load_chain_tasks(conn: &rusqlite::Connection, chain_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT ct.task_id,ct.relationship,ct.created_at,ct.resolved_at,ct.metadata,
                ft.status,ft.title,ft.project_id
         FROM conversation_chain_tasks ct
         LEFT JOIN fleet_tasks ft ON ft.id=ct.task_id
         WHERE ct.chain_id=?1
         ORDER BY ct.created_at ASC",
    )?;
    let rows = stmt.query_map(params![chain_id], |row| {
        let metadata: String = row.get(4)?;
        Ok(json!({
            "task_id":      row.get::<_, String>(0)?,
            "relationship": row.get::<_, String>(1)?,
            "created_at":   row.get::<_, String>(2)?,
            "resolved_at":  row.get::<_, Option<String>>(3)?,
            "metadata":     serde_json::from_str::<Value>(&metadata).unwrap_or_else(|_| json!({})),
            "status":       row.get::<_, Option<String>>(5)?,
            "title":        row.get::<_, Option<String>>(6)?,
            "project_id":   row.get::<_, Option<String>>(7)?,
        }))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn chain_exists(conn: &rusqlite::Connection, id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) FROM conversation_chains WHERE id=?1",
        params![id],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
}

fn chain_source(conn: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT source FROM conversation_chains WHERE id=?1",
        params![id],
        |row| row.get::<_, String>(0),
    )
    .optional()
}

fn ensure_chain_stub(conn: &rusqlite::Connection, id: &str) -> rusqlite::Result<()> {
    let now = now_ts();
    conn.execute(
        "INSERT OR IGNORE INTO conversation_chains
             (id,source,workspace,channel_id,thread_id,title,status,created_at,updated_at,metadata)
         VALUES (?1,'task','','','',?2,'active',?3,?3,'{}')",
        params![id, id, now],
    )?;
    Ok(())
}

fn upsert_participant_value(
    conn: &rusqlite::Connection,
    chain_id: &str,
    default_platform: &str,
    participant: &Value,
) -> rusqlite::Result<()> {
    match participant {
        Value::String(id) => upsert_participant(
            conn,
            chain_id,
            default_platform,
            id,
            None,
            "human".to_string(),
            &now_ts(),
            &json!({}),
        ),
        Value::Object(_) => {
            let id =
                string_field(participant, &["id", "participant_id", "user_id"]).unwrap_or_default();
            if id.is_empty() {
                return Ok(());
            }
            let platform = string_field(participant, &["platform"])
                .unwrap_or_else(|| default_platform.to_string());
            let name = string_field(participant, &["name", "display_name"]);
            let kind = string_field(participant, &["kind", "participant_kind"])
                .unwrap_or_else(|| "human".to_string());
            let metadata = participant
                .get("metadata")
                .cloned()
                .unwrap_or_else(|| json!({}));
            upsert_participant(
                conn,
                chain_id,
                &platform,
                &id,
                name,
                kind,
                &now_ts(),
                &metadata,
            )
        }
        _ => Ok(()),
    }
}

fn upsert_participant(
    conn: &rusqlite::Connection,
    chain_id: &str,
    platform: &str,
    participant_id: &str,
    display_name: Option<String>,
    participant_kind: String,
    seen_at: &str,
    metadata: &Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO conversation_chain_participants
             (chain_id,participant_id,platform,display_name,participant_kind,first_seen_at,last_seen_at,metadata)
         VALUES (?1,?2,?3,?4,?5,?6,?6,?7)
         ON CONFLICT(chain_id, participant_id) DO UPDATE SET
             platform=CASE WHEN excluded.platform != '' THEN excluded.platform ELSE platform END,
             display_name=COALESCE(excluded.display_name, display_name),
             participant_kind=excluded.participant_kind,
             last_seen_at=excluded.last_seen_at,
             metadata=CASE WHEN excluded.metadata != '{}' THEN excluded.metadata ELSE metadata END",
        params![
            chain_id,
            participant_id,
            platform,
            display_name,
            participant_kind,
            seen_at,
            json_string(metadata),
        ],
    )?;
    Ok(())
}

fn upsert_entity_value(
    conn: &rusqlite::Connection,
    chain_id: &str,
    entity: &Value,
) -> rusqlite::Result<()> {
    match entity {
        Value::String(id) => upsert_entity(
            conn,
            chain_id,
            "tag",
            id,
            Some(id.clone()),
            &now_ts(),
            &json!({}),
        ),
        Value::Object(_) => {
            let entity_type =
                string_field(entity, &["type", "entity_type"]).unwrap_or_else(|| "tag".to_string());
            let entity_id = string_field(entity, &["id", "entity_id"]).unwrap_or_default();
            if entity_id.is_empty() {
                return Ok(());
            }
            let label = string_field(entity, &["label", "name"]);
            let metadata = entity.get("metadata").cloned().unwrap_or_else(|| json!({}));
            upsert_entity(
                conn,
                chain_id,
                &entity_type,
                &entity_id,
                label,
                &now_ts(),
                &metadata,
            )
        }
        _ => Ok(()),
    }
}

fn upsert_entity(
    conn: &rusqlite::Connection,
    chain_id: &str,
    entity_type: &str,
    entity_id: &str,
    label: Option<String>,
    seen_at: &str,
    metadata: &Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO conversation_chain_entities
             (chain_id,entity_type,entity_id,label,first_seen_at,last_seen_at,metadata)
         VALUES (?1,?2,?3,?4,?5,?5,?6)
         ON CONFLICT(chain_id, entity_type, entity_id) DO UPDATE SET
             label=COALESCE(excluded.label, label),
             last_seen_at=excluded.last_seen_at,
             metadata=CASE WHEN excluded.metadata != '{}' THEN excluded.metadata ELSE metadata END",
        params![
            chain_id,
            entity_type,
            entity_id,
            label,
            seen_at,
            json_string(metadata)
        ],
    )?;
    Ok(())
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn json_string(value: &Value) -> String {
    if value.is_object() || value.is_array() {
        value.to_string()
    } else {
        json!({ "value": value }).to_string()
    }
}

fn compact_title(text: &str) -> String {
    let mut title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.len() > 120 {
        title.truncate(117);
        title.push_str("...");
    }
    title
}

fn is_terminal_status(status: &str) -> bool {
    matches!(
        status,
        "closed" | "success" | "failure" | "abandoned" | "resolved" | "unresolved" | "partial"
    )
}

fn chain_uuid(prefix: &str) -> String {
    format!(
        "{}-{}",
        prefix,
        uuid::Uuid::new_v4().to_string().replace('-', "")
    )
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn server_error<E: std::fmt::Display>(e: E) -> axum::response::Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
    )
        .into_response()
}
