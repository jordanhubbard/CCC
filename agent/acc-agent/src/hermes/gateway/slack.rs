use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::Instrument;

use super::super::agent::HermesAgent;
use super::session::SessionStore;
use acc_client::Client;

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
    client: Client,
    workspace: String,
    sessions: Arc<SessionStore>,
    agent: Arc<HermesAgent>,
    active: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    seen_replies: Arc<Mutex<HashSet<String>>>,
}

impl SlackAdapter {
    /// Build a SlackAdapter from already-resolved tokens. The gateway is
    /// responsible for fetching tokens (secret store with env-var fallback);
    /// this constructor only validates the bot token via `auth.test` to
    /// derive the bot user ID used for mention detection.
    ///
    /// Returns None when `auth.test` fails — the caller logs the workspace
    /// label and continues without Slack on this gateway.
    pub async fn new(
        sessions: Arc<SessionStore>,
        agent: Arc<HermesAgent>,
        client: Client,
        workspace: String,
        bot_token: String,
        app_token: String,
    ) -> Option<Self> {
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
            client,
            workspace,
            sessions,
            agent,
            active: Arc::new(Mutex::new(HashMap::new())),
            seen_replies: Arc::new(Mutex::new(HashSet::new())),
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
        if event_type == "reaction_added" {
            self.handle_reaction(event).await;
            return;
        }

        let user = event["user"]
            .as_str()
            .or_else(|| event["bot_id"].as_str())
            .unwrap_or("");

        // Ignore our own messages. Other bots are recorded as context, but we
        // don't respond to them.
        if user == self.bot_user_id {
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
        if raw_text.is_empty() && event_type == "message" {
            return;
        }
        let mention = format!("<@{}>", self.bot_user_id);
        let mentions_self = raw_text.contains(&mention);

        let root_ts = thread_ts.clone().unwrap_or_else(|| msg_ts.clone());
        let chain_id = slack_chain_id(&self.workspace, &channel, &root_ts);
        self.record_chain_event(
            &chain_id,
            json!({
                "id": chain_id,
                "source": "slack",
                "workspace": self.workspace,
                "channel_id": channel,
                "thread_id": root_ts,
                "root_event_id": root_ts,
                "participants": [{
                    "id": user,
                    "platform": "slack",
                    "kind": if event["bot_id"].is_string() { "bot" } else { "human" }
                }],
                "entities": [{
                    "type": "slack_channel",
                    "id": channel
                }]
            }),
            json!({
                "event_type": "message",
                "source": "slack",
                "source_event_id": msg_ts,
                "actor_id": user,
                "actor_kind": if event["bot_id"].is_string() { "bot" } else { "human" },
                "text": raw_text,
                "occurred_at": slack_ts_to_rfc3339(event["ts"].as_str()),
                "participants": [{
                    "id": user,
                    "platform": "slack",
                    "kind": if event["bot_id"].is_string() { "bot" } else { "human" }
                }],
                "entities": [{
                    "type": "slack_channel",
                    "id": channel
                }],
                "metadata": {
                    "channel_type": channel_type,
                    "thread_ts": event["thread_ts"],
                    "slack_type": event_type
                }
            }),
        )
        .await;

        if event["bot_id"].is_string() && !mentions_self {
            return;
        }

        // Determine if we should respond.
        let (should_respond, clean_text) = match event_type {
            "app_mention" => {
                // Strip the @mention prefix.
                let clean = raw_text.replace(&mention, "").trim().to_string();
                (true, clean)
            }
            "message" if raw_text.contains(&mention) => {
                // Some Slack app configurations receive only the channel
                // message event for mentions. Treat an explicit mention of
                // this bot as actionable even without a separate app_mention
                // event.
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
        if !self.claim_reply_once(&channel, &msg_ts).await {
            return;
        }

        // Handle /reset.
        if clean_text.trim() == "/reset" {
            let key = session_key(&channel, user, channel_type);
            self.sessions.clear(&key).await;
            let _ = self
                .post_message(&channel, thread_ts.as_deref(), "Conversation reset.")
                .await;
            self.record_chain_event(
                &chain_id,
                json!({"id": chain_id, "source": "slack", "workspace": self.workspace, "channel_id": channel, "thread_id": root_ts}),
                json!({
                    "event_type": "session_reset",
                    "source": "slack",
                    "actor_id": user,
                    "actor_kind": "human",
                    "text": "/reset"
                }),
            ).await;
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
        self.record_chain_event(
            &chain_id,
            json!({"id": chain_id, "source": "slack", "workspace": self.workspace, "channel_id": channel, "thread_id": root_ts}),
            json!({
                "event_type": if response.starts_with("I encountered an error:") { "error" } else { "bot_reply" },
                "source": "slack",
                "actor_id": self.bot_user_id,
                "actor_kind": "bot",
                "text": response,
                "metadata": {
                    "trace_id": trace_id
                }
            }),
        ).await;
    }

    async fn claim_reply_once(&self, channel: &str, msg_ts: &str) -> bool {
        let key = format!("{}:{channel}:{msg_ts}:{}", self.workspace, self.bot_user_id);
        let mut seen = self.seen_replies.lock().await;
        if seen.contains(&key) {
            return false;
        }
        if seen.len() > 2_000 {
            seen.clear();
        }
        seen.insert(key);
        true
    }

    async fn handle_reaction(&self, event: &Value) {
        let user = event["user"].as_str().unwrap_or("");
        if user == self.bot_user_id || user.is_empty() {
            return;
        }
        let channel = event["item"]["channel"].as_str().unwrap_or("");
        let item_ts = event["item"]["ts"].as_str().unwrap_or("");
        if channel.is_empty() || item_ts.is_empty() {
            return;
        }
        let chain_id = slack_chain_id(&self.workspace, channel, item_ts);
        let reaction = event["reaction"].as_str().unwrap_or("");
        self.record_chain_event(
            &chain_id,
            json!({
                "id": chain_id,
                "source": "slack",
                "workspace": self.workspace,
                "channel_id": channel,
                "thread_id": item_ts,
                "root_event_id": item_ts,
                "participants": [{
                    "id": user,
                    "platform": "slack",
                    "kind": "human"
                }],
                "entities": [{
                    "type": "slack_channel",
                    "id": channel
                }]
            }),
            json!({
                "event_type": "reaction",
                "source": "slack",
                "source_event_id": format!("reaction:{channel}:{item_ts}:{user}:{reaction}"),
                "actor_id": user,
                "actor_kind": "human",
                "text": format!(":{reaction}:"),
                "occurred_at": slack_ts_to_rfc3339(event["event_ts"].as_str()),
                "metadata": {
                    "reaction": reaction,
                    "item_ts": item_ts,
                    "item_type": event["item"]["type"]
                }
            }),
        )
        .await;
    }

    async fn record_chain_event(&self, chain_id: &str, chain: Value, event: Value) {
        if let Err(e) = self.client.chains().upsert(&chain).await {
            tracing::warn!("[slack] chain upsert failed for {chain_id}: {e}");
            return;
        }
        if let Err(e) = self.client.chains().append_event(chain_id, &event).await {
            tracing::warn!("[slack] chain event append failed for {chain_id}: {e}");
        }
    }

    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<Option<String>, String> {
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
        Ok(resp["ts"].as_str().map(str::to_string))
    }
}

fn session_key(channel: &str, user: &str, channel_type: &str) -> String {
    if channel_type == "im" {
        format!("slack_dm_{channel}")
    } else {
        format!("slack_{channel}_{user}")
    }
}

fn slack_chain_id(workspace: &str, channel: &str, root_ts: &str) -> String {
    safe_chain_id(&["slack", workspace, channel, root_ts])
}

fn safe_chain_id(parts: &[&str]) -> String {
    let mut id = parts
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .trim_matches('-')
                .to_ascii_lowercase()
        })
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if !id.starts_with("chain-") {
        id = format!("chain-{id}");
    }
    id
}

fn slack_ts_to_rfc3339(ts: Option<&str>) -> String {
    let secs = ts
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
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
