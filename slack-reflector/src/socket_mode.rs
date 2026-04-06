use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{debug, error, info, warn};

/// A message event extracted from Socket Mode
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub workspace_idx: usize,
    pub channel_id: String,
    pub user_id: String,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    /// The envelope_id we need to acknowledge
    pub envelope_id: String,
}

/// Request a WebSocket URL via apps.connections.open
async fn get_ws_url(app_token: &str) -> Result<String> {
    let client = Client::new();
    let resp: Value = client
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .send()
        .await?
        .json()
        .await?;

    if resp["ok"].as_bool() != Some(true) {
        bail!(
            "apps.connections.open failed: {}",
            resp["error"].as_str().unwrap_or("unknown")
        );
    }

    resp["url"]
        .as_str()
        .map(|s| s.to_string())
        .context("No url in connections.open response")
}

/// Connect to Socket Mode and stream message events into tx.
/// Reconnects automatically on disconnect.
pub async fn run_socket_mode(
    workspace_idx: usize,
    workspace_name: String,
    app_token: String,
    tx: mpsc::Sender<IncomingMessage>,
) {
    loop {
        match run_socket_mode_inner(workspace_idx, &workspace_name, &app_token, &tx).await {
            Ok(_) => {
                info!("[{}] Socket Mode connection closed, reconnecting...", workspace_name);
            }
            Err(e) => {
                error!("[{}] Socket Mode error: {:#}, reconnecting in 5s...", workspace_name, e);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    }
}

async fn run_socket_mode_inner(
    workspace_idx: usize,
    workspace_name: &str,
    app_token: &str,
    tx: &mpsc::Sender<IncomingMessage>,
) -> Result<()> {
    let ws_url = get_ws_url(app_token).await?;
    info!("[{}] connecting to Socket Mode...", workspace_name);

    let (ws_stream, _) = connect_async(&ws_url)
        .await
        .context("WebSocket connect failed")?;

    info!("[{}] Socket Mode connected", workspace_name);

    let (mut write, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("[{}] WebSocket read error: {}", workspace_name, e);
                break;
            }
        };

        match msg {
            WsMessage::Text(text) => {
                let envelope: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let envelope_id = envelope["envelope_id"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();

                // Always acknowledge the envelope immediately
                if !envelope_id.is_empty() {
                    let ack = serde_json::json!({ "envelope_id": &envelope_id });
                    if let Err(e) = write
                        .send(WsMessage::Text(ack.to_string().into()))
                        .await
                    {
                        warn!("[{}] failed to send ack: {}", workspace_name, e);
                    }
                }

                // We only care about events_api type with message events
                let evt_type = envelope["type"].as_str().unwrap_or("");
                if evt_type != "events_api" {
                    debug!("[{}] ignoring envelope type: {}", workspace_name, evt_type);
                    continue;
                }

                let event = &envelope["payload"]["event"];
                let event_type = event["type"].as_str().unwrap_or("");

                if event_type != "message" {
                    debug!("[{}] ignoring event type: {}", workspace_name, event_type);
                    continue;
                }

                // Skip message subtypes (edits, joins, bot_message, etc.)
                // We only want plain user messages
                if event.get("subtype").is_some() {
                    debug!("[{}] ignoring message subtype: {:?}", workspace_name, event["subtype"]);
                    continue;
                }

                let channel_id = event["channel"].as_str().unwrap_or("").to_string();
                let user_id = event["user"].as_str().unwrap_or("").to_string();
                let text_content = event["text"].as_str().unwrap_or("").to_string();
                let ts = event["ts"].as_str().unwrap_or("").to_string();
                let thread_ts = event["thread_ts"].as_str().map(|s| s.to_string());

                if user_id.is_empty() || text_content.is_empty() {
                    continue;
                }

                debug!(
                    "[{}] message: user={} channel={} ts={} thread={:?}",
                    workspace_name, user_id, channel_id, ts, thread_ts
                );

                let incoming = IncomingMessage {
                    workspace_idx,
                    channel_id,
                    user_id,
                    text: text_content,
                    ts,
                    thread_ts,
                    envelope_id,
                };

                if tx.send(incoming).await.is_err() {
                    error!("[{}] message channel closed, exiting", workspace_name);
                    return Ok(());
                }
            }
            WsMessage::Ping(data) => {
                let _ = write.send(WsMessage::Pong(data)).await;
            }
            WsMessage::Close(_) => {
                info!("[{}] WebSocket closed by server", workspace_name);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
