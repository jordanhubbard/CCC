/// PTY-over-WebSocket: one terminal pane per agent.
///
/// GET /api/panes/:agent/ws?token=<bearer>
///   Upgrades to WebSocket, spawns `ssh -tt user@host hermes` in a PTY,
///   and bridges PTY I/O ↔ WebSocket using the same JSON message protocol
///   as webmux (~/Src/webmux):
///
///   Client → Server: {"type":"input","data":"..."} | {"type":"resize","cols":N,"rows":M}
///   Server → Client: {"type":"output","data":"..."} | {"type":"status","state":"connected"|"disconnected"}
use axum::{
    extract::{ws::Message, ws::WebSocket, Path, Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/panes/:agent/ws", get(ws_handler))
}

#[derive(serde::Deserialize)]
struct WsQuery {
    token: Option<String>,
}

enum PtyCmd {
    Input(Vec<u8>),
    Resize(u16, u16),
}

async fn ws_handler(
    Path(agent_name): Path<String>,
    Query(q): Query<WsQuery>,
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = q.token.unwrap_or_default();
    if !is_token_valid(&state, &token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let (ssh_host, ssh_user, ssh_port) = {
        let agents = state.agents.read().await;
        let agent = match agents.as_object().and_then(|m| m.get(&agent_name)) {
            Some(a) => a.clone(),
            None => return StatusCode::NOT_FOUND.into_response(),
        };
        let host = agent
            .get("ssh_host")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let user = agent
            .get("ssh_user")
            .and_then(|v| v.as_str())
            .unwrap_or("root")
            .to_string();
        let port = agent
            .get("ssh_port")
            .and_then(|v| v.as_u64())
            .unwrap_or(22) as u16;
        (host, user, port)
    };

    if ssh_host.is_empty() {
        return (StatusCode::BAD_REQUEST, "Agent has no ssh_host configured").into_response();
    }

    ws.on_upgrade(move |socket| {
        handle_pane(socket, agent_name, ssh_host, ssh_user, ssh_port)
    })
}

/// Check a raw bearer token string against static agent tokens and user token hashes.
fn is_token_valid(state: &AppState, token: &str) -> bool {
    if state.auth_tokens.is_empty() {
        let user_hashes = state.user_token_hashes.read().unwrap();
        if user_hashes.is_empty() {
            return true; // dev mode — no tokens at all
        }
    }
    // Static agent tokens (plaintext constant-time compare)
    use subtle::ConstantTimeEq;
    for valid in &state.auth_tokens {
        let a: &[u8] = token.as_bytes();
        let b: &[u8] = valid.as_bytes();
        if a.len() == b.len() && bool::from(a.ct_eq(b)) {
            return true;
        }
    }
    // User tokens (stored as SHA-256 hash)
    if !token.is_empty() {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let hash = hex::encode(hasher.finalize());
        let user_hashes = state.user_token_hashes.read().unwrap();
        for valid_hash in user_hashes.iter() {
            let a: &[u8] = hash.as_bytes();
            let b: &[u8] = valid_hash.as_bytes();
            if a.len() == b.len() && bool::from(a.ct_eq(b)) {
                return true;
            }
        }
    }
    false
}

async fn handle_pane(
    ws: WebSocket,
    agent_name: String,
    ssh_host: String,
    ssh_user: String,
    ssh_port: u16,
) {
    // PTY output (reader thread → async send task)
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(256);
    // PTY input + resize commands (async recv loop → writer thread)
    let (pty_tx, pty_rx) = std::sync::mpsc::sync_channel::<PtyCmd>(256);

    // Open PTY pair
    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("[panes] PTY open failed for {}: {}", agent_name, e);
            return;
        }
    };

    // Build SSH command
    let mut cmd = CommandBuilder::new("ssh");
    cmd.arg("-tt");
    cmd.args(["-o", "StrictHostKeyChecking=accept-new"]);
    cmd.args(["-o", "ServerAliveInterval=15"]);
    cmd.args(["-o", "ServerAliveCountMax=3"]);
    cmd.args(["-o", "ConnectTimeout=10"]);
    cmd.args(["-p", &ssh_port.to_string()]);
    cmd.args(["-l", &ssh_user]);
    cmd.arg(&ssh_host);
    cmd.arg("hermes");

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("[panes] SSH spawn failed for {}: {}", agent_name, e);
            return;
        }
    };
    // Slave is no longer needed once the child is spawned
    drop(pair.slave);

    let master = pair.master;
    let mut reader = match master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[panes] PTY clone_reader failed: {}", e);
            return;
        }
    };
    let mut writer = match master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("[panes] PTY take_writer failed: {}", e);
            return;
        }
    };

    // Thread 1: PTY reader → out_tx
    let out_tx2 = out_tx.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let msg = json!({"type": "output", "data": data}).to_string();
                    if out_tx2.blocking_send(msg).is_err() {
                        break;
                    }
                }
            }
        }
        // Signal disconnection to the WS side
        let _ = out_tx2
            .blocking_send(json!({"type": "status", "state": "disconnected"}).to_string());
    });

    // Thread 2: pty_rx → PTY writer + resize (master stays in this thread)
    std::thread::spawn(move || {
        for cmd in pty_rx {
            match cmd {
                PtyCmd::Input(data) => {
                    let _ = writer.write_all(&data);
                }
                PtyCmd::Resize(cols, rows) => {
                    let _ = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
        }
    });

    // Split WebSocket into sender and receiver
    let (mut ws_sink, mut ws_stream) = ws.split();

    // Notify client we're connected
    let _ = ws_sink
        .send(Message::Text(
            json!({"type": "status", "state": "connected"}).to_string().into(),
        ))
        .await;

    // Async task: forward PTY output to WebSocket
    let fwd = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if ws_sink.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Main loop: receive WS messages and dispatch to PTY
    while let Some(result) = ws_stream.next().await {
        let msg = match result {
            Ok(m) => m,
            Err(_) => break,
        };
        match msg {
            Message::Text(text) => {
                let v: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match v.get("type").and_then(|t| t.as_str()) {
                    Some("input") => {
                        if let Some(data) = v.get("data").and_then(|d| d.as_str()) {
                            let _ = pty_tx.try_send(PtyCmd::Input(data.as_bytes().to_vec()));
                        }
                    }
                    Some("resize") => {
                        let cols =
                            v.get("cols").and_then(|c| c.as_u64()).unwrap_or(80) as u16;
                        let rows =
                            v.get("rows").and_then(|r| r.as_u64()).unwrap_or(24) as u16;
                        let _ = pty_tx.try_send(PtyCmd::Resize(cols, rows));
                    }
                    _ => {}
                }
            }
            Message::Close(_) | Message::Binary(_) => break,
            _ => {}
        }
    }

    // Clean up: abort the forward task and kill the SSH child
    fwd.abort();
    let _ = child.kill();
    let _ = child.wait();
}
