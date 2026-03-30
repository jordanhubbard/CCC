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

use crate::models::{ServerFrame, Reaction, MessageWire, UnreadCounts};
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
        .route("/api/messages/schedule", post(schedule_message))
        .route("/api/messages/:id", patch(edit_message).delete(del_message))
        // Threads
        .route("/api/messages/:id/thread", get(get_thread))
        .route("/api/messages/:id/reply", post(reply_message))
        // Reactions
        .route("/api/messages/:id/react", post(add_reaction).delete(del_reaction))
        // Attachments (file sharing)
        .route("/api/messages/:id/attachments", post(upload_attachment))
        .route("/api/attachments/:id", get(get_attachment))
        // Search
        .route("/api/search", get(search_messages))
        // Pins
        .route("/api/channels/:id/pins", get(list_pins))
        .route("/api/channels/:id/pins/:msg_id", post(pin_message).delete(unpin_message))
        // Direct Messages
        .route("/api/dms", get(list_dms).post(open_dm))
        // Presence
        .route("/api/presence", get(get_presence))
        // Agents / Presence
        .route("/api/agents", get(list_agents))
        .route("/api/agents/:id/heartbeat", post(agent_heartbeat))
        // Projects
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/:id", get(get_project).patch(update_project).delete(del_project))
        .route("/api/projects/:id/files", get(list_project_files).post(upload_project_file))
        .route("/api/projects/:id/files/:filename", get(get_project_file))
        // Unread counts / read cursors
        .route("/api/unread", get(get_unread))
        .route("/api/channels/:id/read", post(mark_read))
        // Voice transcription proxy
        .route("/api/voice/transcribe", post(voice_transcribe))
        .route("/api/ai/suggest", post(ai_suggest))
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
        // No broadcast needed for delete — clients can handle 404 on next fetch
        let _ = ch;
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
    let msgs: Vec<MessageWire> = state.db.get_messages(channel, limit, q.since)?.into_iter().map(MessageWire::from).collect();
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
    state.hub.broadcast(&ServerFrame::Message { message: MessageWire::from(msg.clone()) });
    // Broadcast empty UnreadUpdate as a "refresh needed" signal; clients re-fetch their own counts
    state.hub.broadcast(&ServerFrame::UnreadUpdate { counts: std::collections::HashMap::new() });
    let wire = MessageWire::from(msg);
    Ok(Json(json!({ "ok": true, "message": wire, "botReply": null })))
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
    if ok {
        use std::time::{SystemTime, UNIX_EPOCH};
        let _edited_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        // MessageEdit broadcast removed — not in ScWsFrame
    }
    Ok(Json(json!({ "ok": ok })))
}

async fn del_message(
    Extension(state): Extension<SharedState>,
    Path(id): Path<i64>,
) -> R<Json<serde_json::Value>> {
    let ok = state.db.delete_message(id)?;
    if ok {
        // MessageDelete broadcast removed — not in ScWsFrame
    }
    Ok(Json(json!({ "ok": ok })))
}

// ── Scheduled messages ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ScheduleMessageBody {
    channel_id: String,
    sender: String,
    text: String,
    deliver_at: String,
}

async fn schedule_message(
    Extension(state): Extension<SharedState>,
    Json(body): Json<ScheduleMessageBody>,
) -> R<Json<serde_json::Value>> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let deliver_at_ts = parse_deliver_at(&body.deliver_at, now_secs)
        .ok_or_else(|| anyhow::anyhow!("could not parse deliver_at: '{}'", body.deliver_at))?;

    static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let id = format!("sched-{}-{}", now_secs, n);

    state.db.insert_scheduled_message(&id, &body.channel_id, &body.sender, &body.text, deliver_at_ts)?;

    Ok(Json(serde_json::json!({ "ok": true, "id": id, "deliver_at_ts": deliver_at_ts })))
}

fn parse_deliver_at(s: &str, now_secs: i64) -> Option<i64> {
    let s = s.trim();

    // "in Xm" or "in Xh"
    if let Some(rest) = s.strip_prefix("in ") {
        let rest = rest.trim();
        if let Some(mins) = rest.strip_suffix('m').and_then(|n| n.trim().parse::<i64>().ok()) {
            return Some(now_secs + mins * 60);
        }
        if let Some(hrs) = rest.strip_suffix('h').and_then(|n| n.trim().parse::<i64>().ok()) {
            return Some(now_secs + hrs * 3600);
        }
    }

    // "HH:MM"
    if s.len() == 5 && s.chars().nth(2) == Some(':') {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if let (Ok(h), Ok(m)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) {
            if h < 24 && m < 60 {
                use chrono::{Utc, Timelike};
                let now = Utc::now();
                let mut candidate = now
                    .with_hour(h as u32).unwrap()
                    .with_minute(m as u32).unwrap()
                    .with_second(0).unwrap()
                    .with_nanosecond(0).unwrap();
                if candidate.timestamp() <= now_secs {
                    candidate = candidate + chrono::Duration::days(1);
                }
                return Some(candidate.timestamp());
            }
        }
    }

    // ISO8601 / RFC3339
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }

    None
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
    let msgs: Vec<MessageWire> = state.db.get_thread(id, limit)?.into_iter().map(MessageWire::from).collect();
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
    state.hub.broadcast(&ServerFrame::Message { message: MessageWire::from(msg.clone()) });
    let wire = MessageWire::from(msg);
    Ok(Json(json!({ "ok": true, "message": wire })))
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
    state.hub.broadcast(&ServerFrame::Reaction { message_id: msg_id, reactions: Reaction::from_map(&map) });
    Ok(Json(json!({ "ok": true, "reactions": map })))
}

async fn del_reaction(
    Extension(state): Extension<SharedState>,
    Path(msg_id): Path<i64>,
    Json(body): Json<ReactBody>,
) -> R<Json<serde_json::Value>> {
    let map = state.db.remove_reaction(msg_id, &body.from, &body.emoji)?;
    state.hub.broadcast(&ServerFrame::Reaction { message_id: msg_id, reactions: Reaction::from_map(&map) });
    Ok(Json(json!({ "ok": true, "reactions": map })))
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
    state.hub.broadcast(&ServerFrame::Presence { agent: agent_id, online: body.status != "offline" });
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

// ── Presence ──────────────────────────────────────────────────────────────────

async fn get_presence(
    Extension(state): Extension<SharedState>,
) -> R<Json<serde_json::Value>> {
    // Proxy to RCC /api/presence (which caches for 30s)
    let rcc_url = std::env::var("RCC_URL").unwrap_or_else(|_| "http://localhost:8789".into());
    let rcc_token = std::env::var("RCC_AUTH_TOKEN").unwrap_or_default();
    let url = format!("{}/api/presence", rcc_url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    match client.get(&url)
        .header("Authorization", format!("Bearer {}", rcc_token))
        .send().await
    {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().await.unwrap_or(json!({}));
            Ok(Json(data))
        }
        _ => {
            // Fallback: build presence from local user/agent heartbeats
            let users = state.db.get_users().unwrap_or_default();
            let now = chrono::Utc::now().timestamp_millis();
            let mut presence = serde_json::Map::new();
            for user in &users {
                let gap_ms = user.last_seen.map(|ls| now - ls).unwrap_or(i64::MAX);
                let status = if gap_ms <= 3 * 60 * 1000 { "online" }
                    else if gap_ms <= 15 * 60 * 1000 { "away" }
                    else { "offline" };
                presence.insert(user.name.clone(), json!({
                    "status": status,
                    "statusText": status,
                    "since": user.last_seen,
                    "host": null,
                    "gpu": null,
                }));
            }
            Ok(Json(json!({ "ok": true, "agents": presence, "fallback": true })))
        }
    }
}

// ── Pins ──────────────────────────────────────────────────────────────────────

async fn list_pins(
    Extension(state): Extension<SharedState>,
    Path(channel_id): Path<String>,
) -> R<Json<serde_json::Value>> {
    let msgs: Vec<MessageWire> = state.db.get_pins(&channel_id)?.into_iter().map(MessageWire::from).collect();
    Ok(Json(json!({ "pins": msgs, "channel": channel_id })))
}

#[derive(Deserialize)]
struct PinBody {
    pinned_by: Option<String>,
}

async fn pin_message(
    Extension(state): Extension<SharedState>,
    Path((channel_id, msg_id)): Path<(String, i64)>,
    body: Option<Json<PinBody>>,
) -> R<Json<serde_json::Value>> {
    let pinned_by = body.as_ref().and_then(|b| b.pinned_by.as_deref()).unwrap_or("unknown");
    let ok = state.db.pin_message(&channel_id, msg_id, pinned_by)?;
    Ok(Json(json!({ "ok": ok, "channel": channel_id, "message_id": msg_id })))
}

async fn unpin_message(
    Extension(state): Extension<SharedState>,
    Path((channel_id, msg_id)): Path<(String, i64)>,
) -> R<Json<serde_json::Value>> {
    let ok = state.db.unpin_message(&channel_id, msg_id)?;
    Ok(Json(json!({ "ok": ok })))
}

// ── Search ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    channel: Option<String>,
    limit: Option<i64>,
}

async fn search_messages(
    Extension(state): Extension<SharedState>,
    Query(q): Query<SearchQuery>,
) -> R<Json<serde_json::Value>> {
    if q.q.trim().is_empty() {
        return Ok(Json(json!({ "results": [], "count": 0 })));
    }
    let limit = q.limit.unwrap_or(30).min(100);
    // FTS5 query — escape asterisks for prefix matching
    let fts_query = format!("\"{}\"*", q.q.replace('"', "\"\""));
    let msgs = match state.db.search_messages(&fts_query, q.channel.as_deref(), limit) {
        Ok(m) => m,
        Err(_) => {
            // Fallback: plain LIKE search if FTS fails (e.g. special chars)
            let like_query = format!("%{}%", q.q);
            state.db.search_messages(&like_query, q.channel.as_deref(), limit)
                .unwrap_or_default()
        }
    };
    let wires: Vec<MessageWire> = msgs.into_iter().map(MessageWire::from).collect();
    let count = wires.len();
    Ok(Json(json!({ "results": wires, "count": count, "query": q.q })))
}

// ── Attachments ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AttachmentMeta {
    filename: String,
    mime_type: Option<String>,
    /// Base64-encoded content
    content_b64: String,
    from: Option<String>,
}

async fn upload_attachment(
    Extension(state): Extension<SharedState>,
    Path(message_id): Path<i64>,
    Json(body): Json<AttachmentMeta>,
) -> R<Json<serde_json::Value>> {
    use base64::Engine;
    let content = base64::engine::general_purpose::STANDARD
        .decode(&body.content_b64)
        .map_err(|e| anyhow::anyhow!("base64 decode error: {e}"))?;
    let mime = body.mime_type.as_deref().unwrap_or("application/octet-stream");
    let att_id = state.db.insert_attachment(message_id, &body.filename, mime, &content)?;

    // Notify connected clients about the new attachment
    if let Ok(Some(msg)) = state.db.get_message(message_id) {
        let atts = state.db.get_attachments(message_id).unwrap_or_default();
        let mut wire = MessageWire::from(msg);
        wire.attachments = atts;
        state.hub.broadcast(&ServerFrame::Message { message: wire });
    }

    Ok(Json(json!({
        "ok": true,
        "attachment_id": att_id,
        "message_id": message_id,
        "filename": body.filename,
        "url": format!("/sc/api/attachments/{}", att_id),
    })))
}

async fn get_attachment(
    Extension(state): Extension<SharedState>,
    Path(att_id): Path<i64>,
) -> Response {
    match state.db.get_attachment_content(att_id) {
        Ok(Some((filename, mime_type, content))) => (
            StatusCode::OK,
            [
                (axum::http::header::CONTENT_TYPE, mime_type),
                (axum::http::header::CONTENT_DISPOSITION, format!("inline; filename=\"{}\"", filename)),
            ],
            content,
        ).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "attachment not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Direct Messages ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DmQuery {
    agent: Option<String>,
}

async fn list_dms(
    Extension(state): Extension<SharedState>,
    Query(q): Query<DmQuery>,
) -> R<Json<serde_json::Value>> {
    let agent = q.agent.as_deref().unwrap_or("");
    let dms = if agent.is_empty() {
        // Return all DM channels (admin view)
        state.db.get_channels()?.into_iter().filter(|c| c.channel_type == "dm").collect()
    } else {
        state.db.get_dms_for_agent(agent)?
    };
    Ok(Json(json!(dms)))
}

#[derive(Deserialize)]
struct OpenDmBody {
    from: String,
    to: String,
}

async fn open_dm(
    Extension(state): Extension<SharedState>,
    Json(body): Json<OpenDmBody>,
) -> R<Json<serde_json::Value>> {
    let ch = state.db.get_or_create_dm(&body.from, &body.to)?;
    state.hub.broadcast(&ServerFrame::Channel { action: "dm_opened".into(), channel: ch.clone() });
    Ok(Json(json!({ "ok": true, "channel": ch })))
}

// ── Unread counts / read cursors ──────────────────────────────────────────────

#[derive(Deserialize)]
struct UnreadQuery {
    user: Option<String>,
}

async fn get_unread(
    Extension(state): Extension<SharedState>,
    Query(q): Query<UnreadQuery>,
) -> R<Json<UnreadCounts>> {
    let user_id = q.user.as_deref().unwrap_or("anonymous");
    let counts = state.db.get_unread_counts(user_id)?;
    Ok(Json(UnreadCounts { counts }))
}

#[derive(Deserialize)]
struct MarkReadBody {
    user: String,
    /// Millisecond timestamp of the last-seen message; defaults to now if omitted
    ts: Option<i64>,
}

async fn mark_read(
    Extension(state): Extension<SharedState>,
    Path(channel_id): Path<String>,
    Json(body): Json<MarkReadBody>,
) -> R<Json<serde_json::Value>> {
    let ts = body.ts.unwrap_or_else(|| {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
    });
    state.db.upsert_read_cursor(&body.user, &channel_id, ts)?;
    // Push updated counts to this user's clients via WS broadcast
    let counts = state.db.get_unread_counts(&body.user)?;
    state.hub.broadcast(&ServerFrame::UnreadUpdate { counts });
    Ok(Json(json!({ "ok": true })))
}

// ── Voice transcription proxy ─────────────────────────────────────────────────

async fn voice_transcribe(
    mut multipart: axum::extract::Multipart,
) -> Response {
    let mut audio_bytes: Option<Vec<u8>> = None;
    let mut filename = "audio.webm".to_string();

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            filename = field.file_name().unwrap_or("audio.webm").to_string();
            if let Ok(bytes) = field.bytes().await {
                audio_bytes = Some(bytes.to_vec());
            }
        }
    }

    let bytes = match audio_bytes {
        Some(b) => b,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "no file field"}))).into_response(),
    };

    let client = reqwest::Client::new();
    let part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
    let part = match part.mime_str("audio/webm") {
        Ok(p) => p,
        Err(_) => reqwest::multipart::Part::bytes(vec![]),
    };
    let form = reqwest::multipart::Form::new().part("file", part);

    match client
        .post("http://sparky.tail407856.ts.net:8792/inference")
        .multipart(form)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(data) => (StatusCode::OK, Json(data)).into_response(),
            Err(_) => (StatusCode::BAD_GATEWAY, Json(json!({"error": "invalid STT response"}))).into_response(),
        },
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "STT unavailable"}))).into_response(),
    }
}

// ── AI Suggest ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AiSuggestBody {
    channel_id: String,
    user_prompt: String,
    message_count: Option<u32>,
}

async fn ai_suggest(
    Extension(state): Extension<SharedState>,
    Json(body): Json<AiSuggestBody>,
) -> Response {
    let count = body.message_count.unwrap_or(10) as i64;
    let msgs = match state.db.get_messages(&body.channel_id, count, None) {
        Ok(m) => m,
        Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "AI service unavailable"}))).into_response(),
    };

    let history: String = msgs.iter().map(|m| {
        format!("{}: {}", m.from_agent, m.text)
    }).collect::<Vec<_>>().join("\n");

    let context = format!(
        "Channel history (last {} messages):\n{}\n\nUser request: {}",
        msgs.len(), history, body.user_prompt
    );

    let api_key = std::env::var("TOKENHUB_API_KEY").unwrap_or_default();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let payload = serde_json::json!({
        "model": "meta/llama-3.3-70b-instruct",
        "messages": [
            {"role": "system", "content": "You are a helpful chat assistant. Draft a reply."},
            {"role": "user", "content": context}
        ],
        "max_tokens": 300
    });

    match client
        .post("http://localhost:8090/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<serde_json::Value>().await {
                Ok(data) => {
                    let suggestion = data["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    (StatusCode::OK, Json(json!({"suggestion": suggestion}))).into_response()
                }
                Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "AI service unavailable"}))).into_response(),
            }
        }
        _ => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "AI service unavailable"}))).into_response(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn default_public() -> String { "public".into() }
