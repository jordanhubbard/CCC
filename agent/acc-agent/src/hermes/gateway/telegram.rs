use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
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
You are a helpful AI assistant accessible via Telegram. \
Be conversational, clear, and concise. \
When code or commands are shown, use proper formatting. \
You have access to tools (bash, read_file, write_file, web_fetch) — use them proactively \
to provide accurate, grounded answers rather than guessing.";

pub struct TelegramAdapter {
    token: String,
    bot_username: String,
    http: reqwest::Client,
    client: Client,
    sessions: Arc<SessionStore>,
    agent: Arc<HermesAgent>,
    /// Per-session mutex to serialize turns within a conversation.
    active: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>>,
}

impl TelegramAdapter {
    /// Returns None if TELEGRAM_BOT_TOKEN is not set.
    pub async fn new(
        sessions: Arc<SessionStore>,
        agent: Arc<HermesAgent>,
        client: Client,
    ) -> Option<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
        if token.is_empty() {
            return None;
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client");

        // Resolve our own bot username so we can detect @mentions in groups.
        let url = format!("https://api.telegram.org/bot{token}/getMe");
        let me: Value = http.get(&url).send().await.ok()?.json().await.ok()?;
        let bot_username = me["result"]["username"].as_str()?.to_string();
        tracing::info!("[telegram] connected as @{bot_username}");

        Some(Self {
            token,
            bot_username,
            http,
            client,
            sessions,
            agent,
            active: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    pub async fn run(&self) {
        let mut offset: i64 = 0;
        loop {
            let updates = match self.poll(offset).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!("[telegram] poll error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            for update in updates {
                let update_id = update["update_id"].as_i64().unwrap_or(0);
                offset = offset.max(update_id + 1);
                if let Some(msg) = update.get("message") {
                    self.handle_message(msg).await;
                } else if let Some(reaction) = update.get("message_reaction") {
                    self.handle_reaction(reaction).await;
                }
            }
        }
    }

    async fn poll(&self, offset: i64) -> Result<Vec<Value>, String> {
        let resp = self
            .http
            .get(self.api_url("getUpdates"))
            .query(&[("timeout", "30"), ("offset", &offset.to_string())])
            .timeout(std::time::Duration::from_secs(35))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let body: Value = resp.json().await.map_err(|e| e.to_string())?;
        if !body["ok"].as_bool().unwrap_or(false) {
            return Err(format!(
                "telegram error: {}",
                body["description"].as_str().unwrap_or("?")
            ));
        }
        Ok(body["result"].as_array().cloned().unwrap_or_default())
    }

    async fn handle_message(&self, msg: &Value) {
        let chat = &msg["chat"];
        let chat_id = match chat["id"].as_i64() {
            Some(id) => id,
            None => return,
        };
        let chat_type = chat["type"].as_str().unwrap_or("private");
        let from_id = msg["from"]["id"].as_i64().unwrap_or(0);
        let text = match msg["text"].as_str() {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };
        let message_id = msg["message_id"].as_i64().unwrap_or(0);
        let root_id = msg["message_thread_id"]
            .as_i64()
            .or_else(|| msg["reply_to_message"]["message_id"].as_i64())
            .unwrap_or(message_id);
        let chain_id = telegram_chain_id(chat_id, root_id);
        let from_name = telegram_display_name(&msg["from"]);
        self.record_chain_event(
            &chain_id,
            json!({
                "id": chain_id,
                "source": "telegram",
                "workspace": "telegram",
                "channel_id": chat_id.to_string(),
                "thread_id": root_id.to_string(),
                "root_event_id": root_id.to_string(),
                "participants": [{
                    "id": from_id.to_string(),
                    "platform": "telegram",
                    "name": from_name,
                    "kind": if msg["from"]["is_bot"].as_bool().unwrap_or(false) { "bot" } else { "human" }
                }],
                "entities": [{
                    "type": "telegram_chat",
                    "id": chat_id.to_string()
                }]
            }),
            json!({
                "event_type": "message",
                "source": "telegram",
                "source_event_id": format!("{chat_id}:{message_id}"),
                "actor_id": from_id.to_string(),
                "actor_name": from_name,
                "actor_kind": if msg["from"]["is_bot"].as_bool().unwrap_or(false) { "bot" } else { "human" },
                "text": text,
                "occurred_at": telegram_date_to_rfc3339(msg["date"].as_i64()),
                "metadata": {
                    "chat_type": chat_type,
                    "message_id": message_id,
                    "root_message_id": root_id
                }
            }),
        ).await;

        // In groups, only respond if the bot is @mentioned.
        let (should_respond, text_clean) = if chat_type == "private" {
            (true, text.to_string())
        } else {
            let mention = format!("@{}", self.bot_username);
            if text.contains(&mention) {
                (true, text.replace(&mention, "").trim().to_string())
            } else {
                (false, String::new())
            }
        };
        if !should_respond || text_clean.is_empty() {
            return;
        }

        // Handle /reset command.
        if text_clean.trim() == "/reset" || text_clean.trim() == "/start" {
            let key = session_key(chat_id, from_id, chat_type);
            self.sessions.clear(&key).await;
            let _ = self
                .send_message(chat_id, None, "Conversation reset.")
                .await;
            self.record_chain_event(
                &chain_id,
                json!({"id": chain_id, "source": "telegram", "workspace": "telegram", "channel_id": chat_id.to_string(), "thread_id": root_id.to_string()}),
                json!({
                    "event_type": "session_reset",
                    "source": "telegram",
                    "actor_id": from_id.to_string(),
                    "actor_kind": "human",
                    "text": text_clean
                }),
            ).await;
            return;
        }

        let session_key = session_key(chat_id, from_id, chat_type);
        let reply_to = msg["message_id"].as_i64();

        // Serialize turns per session.
        let lock = {
            let mut map = self.active.lock().await;
            map.entry(session_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        let trace_id = new_trace_id();
        let span = tracing::info_span!(
            "telegram_turn",
            trace_id = %trace_id,
            chat_id  = %chat_id,
            from_id  = %from_id,
        );

        let mut history = self.sessions.load_history(&session_key).await;
        let response = self
            .agent
            .run_gateway_turn(&mut history, &text_clean, GATEWAY_SYSTEM)
            .instrument(span)
            .await;
        self.sessions.save_history(&session_key, &history).await;

        // Split long messages — Telegram limit is 4096 chars.
        for chunk in split_message(&response, 4096) {
            if let Err(e) = self.send_message(chat_id, reply_to, &chunk).await {
                tracing::warn!("[telegram] send error: {e}");
            }
        }
        self.record_chain_event(
            &chain_id,
            json!({"id": chain_id, "source": "telegram", "workspace": "telegram", "channel_id": chat_id.to_string(), "thread_id": root_id.to_string()}),
            json!({
                "event_type": if response.starts_with("I encountered an error:") { "error" } else { "bot_reply" },
                "source": "telegram",
                "actor_id": self.bot_username,
                "actor_kind": "bot",
                "text": response,
                "metadata": {
                    "trace_id": trace_id
                }
            }),
        ).await;
    }

    async fn handle_reaction(&self, reaction: &Value) {
        let chat_id = match reaction["chat"]["id"].as_i64() {
            Some(id) => id,
            None => return,
        };
        let message_id = reaction["message_id"].as_i64().unwrap_or(0);
        let user_id = reaction["user"]["id"].as_i64().unwrap_or(0);
        if message_id == 0 || user_id == 0 {
            return;
        }
        let chain_id = telegram_chain_id(chat_id, message_id);
        let emoji = reaction["new_reaction"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v["emoji"].as_str())
            .unwrap_or("");
        self.record_chain_event(
            &chain_id,
            json!({
                "id": chain_id,
                "source": "telegram",
                "workspace": "telegram",
                "channel_id": chat_id.to_string(),
                "thread_id": message_id.to_string(),
                "root_event_id": message_id.to_string()
            }),
            json!({
                "event_type": "reaction",
                "source": "telegram",
                "source_event_id": format!("reaction:{chat_id}:{message_id}:{user_id}:{emoji}"),
                "actor_id": user_id.to_string(),
                "actor_kind": "human",
                "text": emoji,
                "occurred_at": telegram_date_to_rfc3339(reaction["date"].as_i64()),
                "metadata": {
                    "message_id": message_id,
                    "emoji": emoji
                }
            }),
        )
        .await;
    }

    async fn record_chain_event(&self, chain_id: &str, chain: Value, event: Value) {
        if let Err(e) = self.client.chains().upsert(&chain).await {
            tracing::warn!("[telegram] chain upsert failed for {chain_id}: {e}");
            return;
        }
        if let Err(e) = self.client.chains().append_event(chain_id, &event).await {
            tracing::warn!("[telegram] chain event append failed for {chain_id}: {e}");
        }
    }

    async fn send_message(
        &self,
        chat_id: i64,
        reply_to: Option<i64>,
        text: &str,
    ) -> Result<(), String> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });
        if let Some(id) = reply_to {
            body["reply_to_message_id"] = serde_json::json!(id);
        }
        let resp = self
            .http
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let result: Value = resp.json().await.map_err(|e| e.to_string())?;
        if !result["ok"].as_bool().unwrap_or(false) {
            // If Markdown fails, retry as plain text.
            if result["description"]
                .as_str()
                .map_or(false, |d| d.contains("parse"))
            {
                let mut plain = body.clone();
                if let Some(obj) = plain.as_object_mut() {
                    obj.remove("parse_mode");
                }
                let _ = self
                    .http
                    .post(self.api_url("sendMessage"))
                    .json(&plain)
                    .send()
                    .await;
            }
            return Err(format!(
                "telegram send error: {}",
                result["description"].as_str().unwrap_or("?")
            ));
        }
        Ok(())
    }
}

fn session_key(chat_id: i64, from_id: i64, chat_type: &str) -> String {
    if chat_type == "private" {
        format!("telegram_{chat_id}")
    } else {
        format!("telegram_{chat_id}_{from_id}")
    }
}

fn telegram_chain_id(chat_id: i64, root_id: i64) -> String {
    safe_chain_id(&["telegram", &chat_id.to_string(), &root_id.to_string()])
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

fn telegram_display_name(user: &Value) -> String {
    let first = user["first_name"].as_str().unwrap_or("");
    let last = user["last_name"].as_str().unwrap_or("");
    let username = user["username"].as_str().unwrap_or("");
    let name = format!("{first} {last}").trim().to_string();
    if !name.is_empty() {
        name
    } else {
        username.to_string()
    }
}

fn telegram_date_to_rfc3339(date: Option<i64>) -> String {
    let secs = date.unwrap_or_else(|| chrono::Utc::now().timestamp());
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

/// Split text into chunks at paragraph or sentence boundaries.
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
