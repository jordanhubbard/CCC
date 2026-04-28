use crate::AppState;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Json,
    },
    routing::{get, post},
    Router,
};
use futures_util::stream::{self, Stream, StreamExt};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio_stream::wrappers::BroadcastStream;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // /api/bus/* — used by dashboard UI and API clients
        .route("/api/bus/stream", get(bus_stream))
        .route("/api/bus/send", post(bus_send))
        .route("/api/bus/messages", get(bus_messages))
        .route("/api/bus/presence", get(bus_presence))
        // /bus/* — used by ClawChat (nginx proxies /bus/ → 8789/bus/)
        .route("/bus/stream", get(bus_stream))
        .route("/bus/send", post(bus_send))
        .route("/bus/messages", get(bus_messages))
        .route("/bus/presence", get(bus_presence))
}

// ── Query params for /bus/messages ────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct BusQuery {
    /// Max number of messages to return (default 500, max 2000).
    limit: Option<usize>,
    /// Filter by subject (channel). Matches exact string.
    subject: Option<String>,
    /// Filter by message type ("text", "reaction", etc.).
    #[serde(rename = "type")]
    msg_type: Option<String>,
    /// Filter replies: return only messages with this thread_id.
    thread_id: Option<String>,
    /// DM filter: combined with `from`, returns messages between two users.
    to: Option<String>,
    /// DM filter peer (used with `to`).
    from: Option<String>,
    /// Return only messages with ts > since (ISO-8601).
    since: Option<String>,
}

// ── SSE stream ────────────────────────────────────────────────────────────────

async fn bus_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let replay = load_bus_messages(&state.bus_log_path, 50, &BusQuery::default()).await;

    let rx = state.bus_tx.subscribe();
    let live = BroadcastStream::new(rx).filter_map(|msg| async move {
        match msg {
            Ok(data) => Some(Ok(Event::default().data(data))),
            Err(_) => None,
        }
    });

    let connected = stream::once(async { Ok(Event::default().data(r#"{"type":"connected"}"#)) });
    let replayed = stream::iter(replay.into_iter().map(|msg| Ok(Event::default().data(msg))));

    let combined = connected.chain(replayed).chain(live);
    Sse::new(combined).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(30))
            .text("ping"),
    )
}

// ── POST /bus/send ────────────────────────────────────────────────────────────

async fn bus_send(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    // Validate mime/enc if sender includes a mime field
    if let Some(mime_str) = body.get("mime").and_then(|v| v.as_str()) {
        let mime: crate::bus_types::MediaType = mime_str.parse().unwrap_or_else(|_| unreachable!());
        if !mime.is_known() {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"unknown_media_type","mime":mime_str,
                            "known_types":crate::bus_types::MediaType::all_known()})),
            )
                .into_response();
        }
        if mime.is_binary() {
            let enc = body.get("enc").and_then(|v| v.as_str()).unwrap_or("");
            if enc != "base64" {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error":"binary_type_requires_base64_enc","mime":mime_str})),
                )
                    .into_response();
            }
        }
    }

    let seq = state.bus_seq.fetch_add(1, Ordering::SeqCst);
    let now = chrono::Utc::now().to_rfc3339();

    let mut msg = body;
    if let Some(obj) = msg.as_object_mut() {
        // Assign stable id if not provided by the sender
        obj.entry("id")
            .or_insert_with(|| json!(format!("msg-{}", seq)));
        obj.insert("seq".into(), json!(seq));
        obj.insert("ts".into(), json!(now));
    }

    let msg_str = serde_json::to_string(&msg).unwrap_or_default();
    let log_line = format!("{}\n", msg_str);
    let _ = append_line(&state.bus_log_path, &log_line).await;
    let _ = state.bus_tx.send(msg_str);

    Json(json!({"ok": true, "message": msg})).into_response()
}

// ── GET /bus/messages ─────────────────────────────────────────────────────────

async fn bus_messages(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<BusQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let limit = q.limit.unwrap_or(500).min(2000);
    let msgs = load_bus_messages(&state.bus_log_path, limit, &q).await;
    let parsed: Vec<Value> = msgs
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    Json(json!(parsed)).into_response()
}

// ── GET /bus/presence ─────────────────────────────────────────────────────────

async fn bus_presence(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let agents = state.agents.read().await;
    let now = chrono::Utc::now();

    let mut presence = serde_json::Map::new();
    if let Some(obj) = agents.as_object() {
        for (name, agent) in obj {
            let last_seen_str = agent
                .get("last_seen")
                .or_else(|| agent.get("lastSeen"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let online = chrono::DateTime::parse_from_rfc3339(last_seen_str)
                .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_seconds() < 600)
                .unwrap_or(false);
            presence.insert(
                name.clone(),
                json!({
                    "status": if online { "online" } else { "offline" },
                    "last_seen": last_seen_str,
                }),
            );
        }
    }
    Json(Value::Object(presence)).into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Initialize the bus sequence counter from the log on startup.
///
/// Reads the last-written message's `seq` field and returns `seq + 1` so
/// the next assigned sequence continues monotonically across server
/// restarts. Previously `bus_seq` was hard-initialized to 0, which caused
/// msg-id collisions with existing log entries every restart and made
/// "newest seq" a meaningless ordering signal.
pub fn initial_bus_seq(path: &str) -> u64 {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    for line in content.lines().rev() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(seq) = v.get("seq").and_then(|s| s.as_u64()) {
                return seq + 1;
            }
        }
    }
    0
}

async fn load_bus_messages(path: &str, limit: usize, q: &BusQuery) -> Vec<String> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Walk the log newest-first, apply filters, take up to `limit` matching
    // messages, then reverse to chronological (oldest-first) for the caller.
    //
    // The previous implementation did `.rev().take(limit*4).rev().take(limit)`,
    // which returns the *oldest* `limit` messages in the most recent
    // `limit*4` line window — i.e. a mid-log slice, not the tail. On a log
    // with N > limit*4 lines, SSE replay and `/bus/messages` both silently
    // served 20-day-old data while live writes continued at the tail.
    let mut matched: Vec<String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .filter(|line| {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                return false;
            };

            if let Some(subj) = &q.subject {
                if v.get("subject").and_then(|s| s.as_str()) != Some(subj.as_str()) {
                    return false;
                }
            }
            if let Some(t) = &q.msg_type {
                if v.get("type").and_then(|s| s.as_str()) != Some(t.as_str()) {
                    return false;
                }
            }
            if let Some(tid) = &q.thread_id {
                if v.get("thread_id").and_then(|s| s.as_str()) != Some(tid.as_str()) {
                    return false;
                }
            }
            if let Some(to_user) = &q.to {
                let msg_to = v.get("to").and_then(|s| s.as_str()).unwrap_or("");
                let msg_from = v.get("from").and_then(|s| s.as_str()).unwrap_or("");
                let from_user = q.from.as_deref().unwrap_or("");
                if !((msg_to == to_user && msg_from == from_user)
                    || (msg_to == from_user && msg_from == to_user))
                {
                    return false;
                }
            }
            if let Some(since) = &q.since {
                let msg_ts = v.get("ts").and_then(|s| s.as_str()).unwrap_or("");
                if msg_ts <= since.as_str() {
                    return false;
                }
            }
            true
        })
        .take(limit)
        .map(|s| s.to_string())
        .collect();

    // Reverse back so callers get chronological order (oldest → newest).
    matched.reverse();
    matched
}

async fn append_line(path: &str, line: &str) -> std::io::Result<()> {
    use tokio::fs::OpenOptions;
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    Ok(())
}
