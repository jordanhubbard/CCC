use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;
use crate::models::ServerFrame;

const HUB_CAPACITY: usize = 1024;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_session_id() -> String {
    format!("sc-{}", SESSION_COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Broadcast hub — clone-cheap via Arc<inner>
#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<String>,
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(HUB_CAPACITY);
        Hub { tx }
    }

    /// Subscribe to all frames.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Broadcast a frame to all connected clients.
    pub fn broadcast(&self, frame: &ServerFrame) {
        let json = serde_json::to_string(frame).unwrap();
        let _ = self.tx.send(json);
    }

    pub fn new_session_id() -> String {
        next_session_id()
    }
}

// ── WebSocket handler helper ──────────────────────────────────────────────────

use axum::{
    extract::{WebSocketUpgrade, ws::{WebSocket, Message as WsMsg}},
    response::IntoResponse,
};
use futures::{StreamExt, SinkExt};
use tracing::{info, warn};
use crate::models::{ClientFrame, ServerFrame as SF};
use crate::SharedState;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::Extension(state): axum::extract::Extension<SharedState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: SharedState) {
    let session_id = Hub::new_session_id();
    info!("WS connected: {}", session_id);

    let (mut sink, mut stream) = socket.split();
    let mut rx = state.hub.subscribe();

    // Send connected frame
    let connected = serde_json::to_string(&SF::Connected { session_id: session_id.clone() }).unwrap();
    if sink.send(WsMsg::Text(connected.into())).await.is_err() {
        return;
    }

    // Spawn outbound pump (hub → socket)
    let mut send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if sink.send(WsMsg::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("WS session lagged {} frames", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Inbound pump (socket → handle client frames)
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                WsMsg::Text(text) => {
                    match serde_json::from_str::<ClientFrame>(&text) {
                        Ok(ClientFrame::Ping) => {
                            // pong — no-op (axum handles WS ping/pong at protocol level)
                        }
                        Ok(ClientFrame::Heartbeat { agent, status }) => {
                            info!("WS heartbeat from {} status={}", agent, status);
                            if let Err(e) = state.db.upsert_heartbeat(&agent, &status) {
                                warn!("heartbeat upsert error: {}", e);
                            }
                            let online = status != "offline";
                            state.hub.broadcast(&SF::Presence { agent, online });
                        }
                        Ok(ClientFrame::Typing { channel, agent, is_typing }) => {
                            state.hub.broadcast(&SF::Typing { channel, agent, is_typing });
                        }
                        Err(e) => {
                            warn!("unknown client frame: {} — {:?}", text, e);
                        }
                    }
                }
                WsMsg::Close(_) => break,
                _ => {}
            }
        }
    });

    // If either task finishes, abort the other
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    info!("WS disconnected: {}", session_id);
}
