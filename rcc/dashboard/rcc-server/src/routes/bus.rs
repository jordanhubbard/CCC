use axum::{
    extract::State,
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
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::io::AsyncWriteExt;
use tokio_stream::wrappers::BroadcastStream;
use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/bus/stream", get(bus_stream))
        .route("/api/bus/send", post(bus_send))
        .route("/api/bus/messages", get(bus_messages))
}

async fn bus_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let replay = load_recent_bus_messages(&state.bus_log_path, 50).await;

    let rx = state.bus_tx.subscribe();
    let live = BroadcastStream::new(rx)
        .filter_map(|msg| async move {
            match msg {
                Ok(data) => Some(Ok(Event::default().data(data))),
                Err(_) => None,
            }
        });

    let connected = stream::once(async {
        Ok(Event::default().data(r#"{"type":"connected"}"#))
    });
    let replayed = stream::iter(replay.into_iter().map(|msg| {
        Ok(Event::default().data(msg))
    }));

    let combined = connected.chain(replayed).chain(live);
    Sse::new(combined).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(30))
            .text("ping"),
    )
}

async fn bus_send(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }

    let seq = state.bus_seq.fetch_add(1, Ordering::SeqCst);
    let now = chrono::Utc::now().to_rfc3339();

    let mut msg = body;
    if let Some(obj) = msg.as_object_mut() {
        obj.insert("seq".into(), json!(seq));
        obj.insert("ts".into(), json!(now));
    }

    let msg_str = serde_json::to_string(&msg).unwrap_or_default();

    let log_line = format!("{}\n", msg_str);
    let _ = append_line(&state.bus_log_path, &log_line).await;

    let _ = state.bus_tx.send(msg_str);

    Json(json!({"ok": true, "message": msg})).into_response()
}

async fn bus_messages(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let msgs = load_recent_bus_messages(&state.bus_log_path, 100).await;
    let parsed: Vec<Value> = msgs.iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    Json(json!(parsed))
}

async fn load_recent_bus_messages(path: &str, limit: usize) -> Vec<String> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    content.lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(limit)
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

async fn append_line(path: &str, line: &str) -> std::io::Result<()> {
    use tokio::fs::OpenOptions;
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path).await?;
    file.write_all(line.as_bytes()).await?;
    Ok(())
}
