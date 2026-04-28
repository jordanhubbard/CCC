use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::Instrument;

use super::super::agent::HermesAgent;
use super::session::SessionStore;

fn new_trace_id() -> String {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let seq = CTR.fetch_add(1, Ordering::Relaxed);
    format!("{ts:016x}{seq:04x}")
}

const GATEWAY_SYSTEM: &str = "\
You are a helpful AI assistant accessible via Slack. \
Be conversational, clear, and concise. \
Format responses with Slack mrkdwn: *bold*, _italic_, `code`, ```code blocks```. \
You have access to tools (bash, read_file, write_file, web_fetch) — use them proactively \
to provide accurate, grounded answers rather than guessing.";

pub struct SlackAdapter {
    app_token: String,
    bot_token: String,
    bot_user_id: String,
    http: reqwest::Client,
    sessions: Arc<SessionStore>,
    agent: Arc<HermesAgent>,
    active: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>>,
}

impl SlackAdapter {
    /// Returns None if required tokens are missing.
    ///
    /// `workspace` is an optional uppercase suffix: None → primary vars,
    /// Some("OFTERRA") → SLACK_APP_TOKEN_OFTERRA, SLACK_BOT_TOKEN_OFTERRA.
    pub async fn new(
        sessions: Arc<SessionStore>,
        agent: Arc<HermesAgent>,
        workspace: Option<&str>,
    ) -> Option<Self> {
        let suffix = workspace
            .map(|w| format!("_{}", w.to_uppercase()))
            .unwrap_or_default();

        let app_token = std::env::var(format!("SLACK_APP_TOKEN{suffix}"))
            .ok()
            .filter(|t| t.starts_with("xapp-"))?;
        // For the default workspace also try the legacy SLACK_OMGJKH_TOKEN name.
        let bot_token = std::env::var(format!("SLACK_BOT_TOKEN{suffix}"))
            .ok()
            .or_else(|| {
                if suffix.is_empty() {
                    std::env::var("SLACK_OMGJKH_TOKEN").ok()
                } else {
                    None
                }
            })
            .filter(|t| !t.is_empty())?;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client");

        // Resolve bot user ID for mention detection.
        let resp: Value = http
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&bot_token)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        if !resp["ok"].as_bool().unwrap_or(false) {
            tracing::error!(
                "[slack] auth.test failed: {}",
                resp["error"].as_str().unwrap_or("?")
            );
            return None;
        }
        let bot_user_id = resp["user_id"].as_str()?.to_string();
        tracing::info!(
            "[slack] connected as {} ({})",
            resp["user"].as_str().unwrap_or("?"),
            bot_user_id
        );

        Some(Self {
            app_token,
            bot_token,
            bot_user_id,
            http,
            sessions,
            agent,
            active: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    pub async fn run(&self) {
        let mut backoff = std::time::Duration::from_secs(2);
        loop {
            match self.connect_and_process().await {
                Ok(()) => {
                    // Clean disconnect (e.g. server asked us to reconnect).
                    backoff = std::time::Duration::from_secs(2);
                }
                Err(e) => {
                    tracing::warn!("[slack] connection error: {e} — reconnecting in {backoff:?}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                }
            }
        }
    }

    async fn connect_and_process(&self) -> Result<(), String> {
        // Open a Socket Mode connection.
        let wss_url = self.open_connection().await?;

        let (ws, _) = tokio_tungstenite::connect_async(&wss_url)
            .await
            .map_err(|e| format!("ws connect: {e}"))?;
        let (mut write, mut read) = ws.split();

        tracing::info!("[slack] socket mode connected");

        while let Some(msg) = read.next().await {
            let msg = msg.map_err(|e| format!("ws read: {e}"))?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Ping(d) => {
                    let _ = write.send(Message::Pong(d)).await;
                    continue;
                }
                Message::Close(_) => return Ok(()),
                _ => continue,
            };

            let envelope: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Acknowledge every envelope immediately.
            if let Some(eid) = envelope["envelope_id"].as_str() {
                let ack = json!({"envelope_id": eid}).to_string();
                let _ = write.send(Message::Text(ack.into())).await;
            }

            match envelope["type"].as_str() {
                Some("hello") => tracing::debug!("[slack] hello received"),
                Some("disconnect") => {
                    tracing::info!("[slack] server requested disconnect");
                    return Ok(());
                }
                Some("events_api") => {
                    if let Some(payload) = envelope.get("payload") {
                        let event = &payload["event"];
                        self.handle_event(event).await;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn open_connection(&self) -> Result<String, String> {
        let resp: Value = self
            .http
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(&self.app_token)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if !resp["ok"].as_bool().unwrap_or(false) {
            return Err(format!(
                "apps.connections.open: {}",
                resp["error"].as_str().unwrap_or("?")
            ));
        }
        resp["url"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "no url in connections.open response".to_string())
    }

    async fn handle_event(&self, event: &Value) {
        let event_type = event["type"].as_str().unwrap_or("");
        let user = event["user"].as_str().unwrap_or("");

        // Ignore bot's own messages and other bots.
        if user == self.bot_user_id || event["bot_id"].is_string() {
            return;
        }
        // Ignore message_changed / message_deleted subtypes.
        if event["subtype"].is_string() {
            return;
        }

        let channel = event["channel"].as_str().unwrap_or("").to_string();
        let thread_ts = event["thread_ts"].as_str().map(|s| s.to_string());
        let msg_ts = event["ts"].as_str().unwrap_or("").to_string();
        let channel_type = event["channel_type"].as_str().unwrap_or("");

        let raw_text = event["text"].as_str().unwrap_or("").to_string();

        // Determine if we should respond.
        let (should_respond, clean_text) = match event_type {
            "app_mention" => {
                // Strip the @mention prefix.
                let mention = format!("<@{}>", self.bot_user_id);
                let clean = raw_text.replace(&mention, "").trim().to_string();
                (true, clean)
            }
            "message" if channel_type == "im" => {
                // DMs — always respond.
                (true, raw_text.clone())
            }
            _ => (false, String::new()),
        };

        if !should_respond || clean_text.is_empty() {
            return;
        }

        // Handle /reset.
        if clean_text.trim() == "/reset" {
            let key = session_key(&channel, user, channel_type);
            self.sessions.clear(&key).await;
            let _ = self
                .post_message(&channel, thread_ts.as_deref(), "Conversation reset.")
                .await;
            return;
        }

        let key = session_key(&channel, user, channel_type);
        // Reply in the same thread (or start a new one from the message ts).
        let reply_thread = thread_ts.or(Some(msg_ts));

        let lock = {
            let mut map = self.active.lock().await;
            map.entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        let trace_id = new_trace_id();
        let span = tracing::info_span!(
            "slack_turn",
            trace_id = %trace_id,
            channel  = %channel,
            user     = %user,
        );

        // Show a thinking indicator.
        // TODO: capture the returned ts and delete/update this message after the response is ready;
        // currently the "_thinking…_" message remains permanently in the channel.
        let _ = self
            .post_message(&channel, reply_thread.as_deref(), "_thinking…_")
            .await;

        let mut history = self.sessions.load_history(&key).await;
        let response = self
            .agent
            .run_gateway_turn(&mut history, &clean_text, GATEWAY_SYSTEM)
            .instrument(span)
            .await;
        self.sessions.save_history(&key, &history).await;

        // Slack limit is 3000 chars per block; split if needed.
        for chunk in split_message(&response, 3000) {
            if let Err(e) = self
                .post_message(&channel, reply_thread.as_deref(), &chunk)
                .await
            {
                tracing::warn!("[slack] post error: {e}");
            }
        }
    }

    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), String> {
        let mut body = json!({
            "channel": channel,
            "text": text,
            "mrkdwn": true,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = json!(ts);
        }
        let resp: Value = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if !resp["ok"].as_bool().unwrap_or(false) {
            return Err(format!(
                "chat.postMessage: {}",
                resp["error"].as_str().unwrap_or("?")
            ));
        }
        Ok(())
    }
}

fn session_key(channel: &str, user: &str, channel_type: &str) -> String {
    if channel_type == "im" {
        format!("slack_dm_{channel}")
    } else {
        format!("slack_{channel}_{user}")
    }
}

fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > limit {
        let split_at = remaining[..limit]
            .rfind("\n\n")
            .or_else(|| remaining[..limit].rfind('\n'))
            .or_else(|| remaining[..limit].rfind(". "))
            .unwrap_or(limit);
        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}
