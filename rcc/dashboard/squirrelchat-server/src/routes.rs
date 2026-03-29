use axum::{
    Router,
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::models::{Reaction, ServerFrame};
use crate::SharedState;
use crate::ws;

// ── Error helper ─────────────────────────────────────────────────────────────

struct AppError(anyhow::Error);
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": self.0.to_string() })),
        ).into_response()
    }
}
impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self { AppError(e.into()) }
}
type R<T> = Result<T, AppError>;

// ── Router ───────────────────────────────────────────────────────────────────

pub fn build_router(state: SharedState) -> Router {
    Router::new()
        // Health
        .route("/health", get(health))
        // WebSocket
        .route("/api/ws", get(ws::ws_handler))
        // Channels
        .route("/api/channels", get(list_channels).post(create_channel))
        .route("/api/channels/:id", delete(del_channel))
        // Messages
        .route("/api/messages", get(list_messages).post(post_message))
        .route("/api/messages/:id", patch(edit_message).delete(del_message))
        // Threads
        .route("/api/messages/:id/thread", get(get_thread))
        .route("/api/messages/:id/reply", post(reply_message))
        // Reactions
        .route("/api/messages/:id/react", post(add_reaction).delete(del_reaction))
        // Agents / Presence
        .route("/api/agents", get(list_agents))
        .route("/api/agents/:id/heartbeat", post(agent_heartbeat))
        // Projects
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/:id", get(get_project).patch(update_project).delete(del_project))
        .route("/api/projects/:id/files", get(list_project_files).post(upload_project_file))
        .route("/api/projects/:id/files/:filename", get(get_project_file))
        .layer(Extension(state))
}

// ── Health ───────────────────────────────────────────────────────────────────

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "version": "2.0.0" }))
}

// ── Channels ─────────────────────────────────────────────────────────────────

async fn list_channels(Extension(state): Extension<SharedState>) -> R<Json<serde_json::Value>> {
    let channels = state.db.get_channels()?;
    Ok(Json(json!(channels)))
}

#[derive(Deserialize)]
struct CreateChannelBody {
    id: String,
    name: String,
    #[serde(rename = "type", default = "default_public")]
    channel_type: String,
    description: Option<String>,
    created_by: Option<String>,
}

async fn create_channel(
    Extension(state): Extension<SharedState>,
    Json(body): Json<CreateChannelBody>,
) -> R<Json<serde_json::Value>> {
    let created_by = body.created_by.as_deref().unwrap_or("rocky");
    state.db.insert_channel(&body.id, &body.name, &body.channel_type, created_by, body.description.as_deref())?;
    if let Some(ch) = state.db.get_channel(&body.id)? {
        state.hub.broadcast(&ServerFrame::Channel { action: "created".into(), channel: ch });
    }
    Ok(Json(json!({ "ok": true, "id": body.id })))
}

async fn del_channel(
    Extension(state): Extension<SharedState>,
    Path(id): Path<String>,
) -> R<Json<serde_json::Value>> {
    let ch = state.db.get_channel(&id)?;
    let deleted = state.db.delete_channel(&id)?;
    if deleted {
        if let Some(channel) = ch {
            state.hub.broadcast(&ServerFrame::Channel { action: "deleted".into(), channel });
        }
        Ok(Json(json!({ "ok": true })))
    } else {
        Ok(Json(json!({ "ok": false, "error": "channel not found" })))
    }
}

// ── Messages ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MsgQuery {
    channel: Option<String>,
    limit: Option<i64>,
    since: Option<i64>,
}

async fn list_messages(
    Extension(state): Extension<SharedState>,
    Query(q): Query<MsgQuery>,
) -> R<Json<serde_json::Value>> {
    let channel = q.channel.as_deref().unwrap_or("general");
    let limit = q.limit.unwrap_or(50).min(200);
    let msgs = state.db.get_messages(channel, limit, q.since)?;
    Ok(Json(json!(msgs)))
}

#[derive(Deserialize)]
struct PostMessageBody {
    from: String,
    text: String,
    channel: Option<String>,
    mentions: Option<Vec<String>>,
}

async fn post_message(
    Extension(state): Extension<SharedState>,
    Json(body): Json<PostMessageBody>,
) -> R<Json<serde_json::Value>> {
    let channel = body.channel.as_deref().unwrap_or("general");
    let mentions = body.mentions.unwrap_or_default();
    let id = state.db.insert_message(&body.from, &body.text, channel, &mentions, None)?;
    let msg = state.db.get_message(id)?.ok_or_else(|| anyhow::anyhow!("insert failed"))?;
    state.hub.broadcast(&ServerFrame::Message { message: msg.clone() });
    Ok(Json(json!({ "ok": true, "message": msg, "botReply": null })))
}

#[derive(Deserialize)]
struct EditMessageBody {
    text: String,
}

async fn edit_message(
    Extension(state): Extension<SharedState>,
    Path(id): Path<i64>,
    Json(body): Json<EditMessageBody>,
) -> R<Json<serde_json::Value>> {
    let ok = state.db.update_message(id, &body.text)?;
    Ok(Json(json!({ "ok": ok })))
}

async fn del_message(
    Extension(state): Extension<SharedState>,
    Path(id): Path<i64>,
) -> R<Json<serde_json::Value>> {
    let ok = state.db.delete_message(id)?;
    Ok(Json(json!({ "ok": ok })))
}

// ── Threads ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ThreadQuery {
    limit: Option<i64>,
}

async fn get_thread(
    Extension(state): Extension<SharedState>,
    Path(id): Path<i64>,
    Query(q): Query<ThreadQuery>,
) -> R<Json<serde_json::Value>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let msgs = state.db.get_thread(id, limit)?;
    Ok(Json(json!(msgs)))
}

#[derive(Deserialize)]
struct ReplyBody {
    from: String,
    text: String,
    mentions: Option<Vec<String>>,
}

async fn reply_message(
    Extension(state): Extension<SharedState>,
    Path(parent_id): Path<i64>,
    Json(body): Json<ReplyBody>,
) -> R<Json<serde_json::Value>> {
    // Look up the channel from the parent message
    let parent = state.db.get_message(parent_id)?
        .ok_or_else(|| anyhow::anyhow!("parent message not found"))?;
    let mentions = body.mentions.unwrap_or_default();
    let id = state.db.insert_message(&body.from, &body.text, &parent.channel, &mentions, Some(parent_id))?;
    let msg = state.db.get_message(id)?.ok_or_else(|| anyhow::anyhow!("insert failed"))?;
    state.hub.broadcast(&ServerFrame::Message { message: msg.clone() });
    Ok(Json(json!({ "ok": true, "message": msg })))
}

// ── Reactions ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ReactBody {
    from: String,
    emoji: String,
}

async fn add_reaction(
    Extension(state): Extension<SharedState>,
    Path(msg_id): Path<i64>,
    Json(body): Json<ReactBody>,
) -> R<Json<serde_json::Value>> {
    let map = state.db.add_reaction(msg_id, &body.from, &body.emoji)?;
    let reactions = Reaction::from_map(&map);
    state.hub.broadcast(&ServerFrame::Reaction { message_id: msg_id, reactions: reactions.clone() });
    Ok(Json(json!({ "ok": true, "reactions": reactions })))
}

async fn del_reaction(
    Extension(state): Extension<SharedState>,
    Path(msg_id): Path<i64>,
    Json(body): Json<ReactBody>,
) -> R<Json<serde_json::Value>> {
    let map = state.db.remove_reaction(msg_id, &body.from, &body.emoji)?;
    let reactions = Reaction::from_map(&map);
    state.hub.broadcast(&ServerFrame::Reaction { message_id: msg_id, reactions: reactions.clone() });
    Ok(Json(json!({ "ok": true, "reactions": reactions })))
}

// ── Agents ────────────────────────────────────────────────────────────────────

async fn list_agents(Extension(state): Extension<SharedState>) -> R<Json<serde_json::Value>> {
    let users = state.db.get_users()?;
    Ok(Json(json!(users)))
}

#[derive(Deserialize)]
struct HeartbeatBody {
    status: String,
}

async fn agent_heartbeat(
    Extension(state): Extension<SharedState>,
    Path(agent_id): Path<String>,
    Json(body): Json<HeartbeatBody>,
) -> R<Json<serde_json::Value>> {
    state.db.upsert_heartbeat(&agent_id, &body.status)?;
    let online = body.status != "offline";
    state.hub.broadcast(&ServerFrame::Presence { agent: agent_id, online });
    Ok(Json(json!({ "ok": true })))
}

// ── Projects ──────────────────────────────────────────────────────────────────

async fn list_projects(Extension(state): Extension<SharedState>) -> R<Json<serde_json::Value>> {
    let projects = state.db.get_projects()?;
    Ok(Json(json!(projects)))
}

#[derive(Deserialize)]
struct CreateProjectBody {
    id: String,
    name: String,
    description: Option<String>,
    tags: Option<Vec<String>>,
    assignee: Option<String>,
    status: Option<String>,
}

async fn create_project(
    Extension(state): Extension<SharedState>,
    Json(body): Json<CreateProjectBody>,
) -> R<Json<serde_json::Value>> {
    let tags = body.tags.unwrap_or_default();
    let status = body.status.as_deref().unwrap_or("active");
    state.db.insert_project(&body.id, &body.name, body.description.as_deref(), &tags, body.assignee.as_deref(), status)?;
    Ok(Json(json!({ "ok": true, "id": body.id })))
}

async fn get_project(
    Extension(state): Extension<SharedState>,
    Path(id): Path<String>,
) -> R<Json<serde_json::Value>> {
    let p = state.db.get_project(&id)?;
    Ok(Json(json!(p)))
}

#[derive(Deserialize)]
struct UpdateProjectBody {
    name: Option<String>,
    description: Option<String>,
    status: Option<String>,
    assignee: Option<String>,
}

async fn update_project(
    Extension(state): Extension<SharedState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateProjectBody>,
) -> R<Json<serde_json::Value>> {
    let ok = state.db.update_project(
        &id,
        body.name.as_deref(),
        body.description.as_deref(),
        body.status.as_deref(),
        body.assignee.as_deref(),
    )?;
    Ok(Json(json!({ "ok": ok })))
}

async fn del_project(
    Extension(state): Extension<SharedState>,
    Path(id): Path<String>,
) -> R<Json<serde_json::Value>> {
    let ok = state.db.delete_project(&id)?;
    Ok(Json(json!({ "ok": ok })))
}

// ── Project Files ─────────────────────────────────────────────────────────────

async fn list_project_files(
    Extension(state): Extension<SharedState>,
    Path(project_id): Path<String>,
) -> R<Json<serde_json::Value>> {
    let files = state.db.get_project_files(&project_id)?;
    Ok(Json(json!(files)))
}

async fn upload_project_file(
    Extension(_state): Extension<SharedState>,
    Path(_project_id): Path<String>,
    _body: axum::body::Bytes,
) -> R<Json<serde_json::Value>> {
    Ok(Json(json!({ "ok": false, "error": "use /api/projects/:id/files/:filename PUT" })))
}

async fn get_project_file(
    Extension(state): Extension<SharedState>,
    Path((project_id, filename)): Path<(String, String)>,
) -> Response {
    match state.db.get_project_file_content(&project_id, &filename) {
        Ok(Some(content)) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_DISPOSITION, format!("inline; filename=\"{}\"", filename))],
            content,
        ).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn default_public() -> String { "public".into() }
