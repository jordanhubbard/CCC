use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
/// rcc-server/src/brain.rs — CCC LLM Request Queue + Retry Engine (Rust port of brain/index.mjs)
///
/// Runs as a tokio background task. Accepts requests via the shared BrainQueue.
/// Routes all calls through tokenhub (localhost:8090).
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Types ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainRequest {
    pub id: String,
    pub messages: Vec<Value>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_priority")]
    pub priority: String,
    pub created: String,
    #[serde(default)]
    pub attempts: Vec<Value>,
    #[serde(default = "default_status")]
    pub status: String,
    pub result: Option<String>,
    pub completed_at: Option<String>,
    pub callback_url: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

fn default_max_tokens() -> u32 {
    1024
}
fn default_priority() -> String {
    "normal".to_string()
}
fn default_status() -> String {
    "pending".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrainState {
    pub queue: Vec<BrainRequest>,
    pub completed: Vec<BrainRequest>,
    pub tick_count: u64,
    pub last_tick: Option<String>,
}

// ── Shared Brain Queue ────────────────────────────────────────────────────

pub struct BrainQueue {
    pub state: RwLock<BrainState>,
    pub state_path: String,
    pub tokenhub_url: String,
    pub tokenhub_key: String,
    pub models: Vec<String>,
    pub tick_ms: u64,
    // Notify the worker when a new request is enqueued
    pub notify: tokio::sync::Notify,
}

impl BrainQueue {
    pub fn new() -> Self {
        let tokenhub_url =
            std::env::var("TOKENHUB_URL").unwrap_or_else(|_| "http://localhost:8090".to_string());
        let tokenhub_key = std::env::var("TOKENHUB_AGENT_KEY")
            .or_else(|_| std::env::var("TOKENHUB_API_KEY"))
            .unwrap_or_default();
        let models: Vec<String> = std::env::var("BRAIN_MODELS")
            .unwrap_or_else(|_| {
                "nemotron,peabody-vllm,sherman-vllm,snidely-vllm,dudley-vllm,llama-3.3-70b-instruct"
                    .to_string()
            })
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let state_path = std::env::var("BRAIN_STATE_PATH")
            .unwrap_or_else(|_| "./data/brain-state.json".to_string());
        let tick_ms = std::env::var("BRAIN_TICK_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30_000u64);

        BrainQueue {
            state: RwLock::new(BrainState::default()),
            state_path,
            tokenhub_url,
            tokenhub_key,
            models,
            tick_ms,
            notify: tokio::sync::Notify::new(),
        }
    }

    pub async fn load(&self) {
        match tokio::fs::read_to_string(&self.state_path).await {
            Ok(content) => {
                if let Ok(s) = serde_json::from_str::<BrainState>(&content) {
                    *self.state.write().await = s;
                    info!("brain: loaded state from {}", self.state_path);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("brain: failed to load state: {}", e),
        }
    }

    pub async fn save(&self) {
        let state = self.state.read().await;
        if let Ok(json) = serde_json::to_string_pretty(&*state) {
            drop(state);
            let tmp = format!("{}.tmp", self.state_path);
            if let Some(parent) = std::path::Path::new(&self.state_path).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            if tokio::fs::write(&tmp, &json).await.is_ok() {
                let _ = tokio::fs::rename(&tmp, &self.state_path).await;
            }
        }
    }

    pub async fn enqueue(&self, req: BrainRequest) -> String {
        let id = req.id.clone();
        let mut state = self.state.write().await;
        state.queue.push(req);
        // Sort: high > normal > low, then by created
        state.queue.sort_by(|a, b| {
            let po = priority_order(&a.priority).cmp(&priority_order(&b.priority));
            if po != std::cmp::Ordering::Equal {
                return po;
            }
            a.created.cmp(&b.created)
        });
        drop(state);
        self.save().await;
        self.notify.notify_one();
        info!("brain: enqueued {}", id);
        id
    }

    pub async fn status(&self) -> Value {
        let state = self.state.read().await;
        json!({
            "ok": true,
            "backend": "tokenhub",
            "url": self.tokenhub_url,
            "queueDepth": state.queue.len(),
            "completedCount": state.completed.len(),
            "lastTick": state.last_tick,
            "tickCount": state.tick_count,
        })
    }

    pub async fn tick(&self, client: &reqwest::Client) {
        let pending_ids: Vec<String> = {
            let state = self.state.read().await;
            state
                .queue
                .iter()
                .filter(|r| r.status == "pending")
                .map(|r| r.id.clone())
                .collect()
        };

        if pending_ids.is_empty() {
            let mut state = self.state.write().await;
            state.tick_count += 1;
            state.last_tick = Some(chrono::Utc::now().to_rfc3339());
            return;
        }

        info!(
            "brain: tick — processing {} pending request(s)",
            pending_ids.len()
        );

        for id in pending_ids {
            self.process_request(&id, client).await;
            self.save().await;
        }

        // Trim completed to last 100
        let mut state = self.state.write().await;
        state.tick_count += 1;
        state.last_tick = Some(chrono::Utc::now().to_rfc3339());
        if state.completed.len() > 100 {
            let len = state.completed.len();
            state.completed.drain(0..len - 100);
        }
    }

    async fn process_request(&self, id: &str, client: &reqwest::Client) {
        // Mark in-progress
        {
            let mut state = self.state.write().await;
            if let Some(r) = state.queue.iter_mut().find(|r| r.id == id) {
                r.status = "in-progress".to_string();
            }
        }

        let (messages, max_tokens, callback_url) = {
            let state = self.state.read().await;
            match state.queue.iter().find(|r| r.id == id) {
                Some(r) => (r.messages.clone(), r.max_tokens, r.callback_url.clone()),
                None => return,
            }
        };

        let attempt_ts = chrono::Utc::now().to_rfc3339();
        let result = self.call_model(client, &messages, max_tokens).await;

        let mut state = self.state.write().await;
        let req_opt = state.queue.iter_mut().find(|r| r.id == id);
        if let Some(req) = req_opt {
            match result {
                Ok((text, tokens_used)) => {
                    req.attempts.push(json!({
                        "model": "tokenhub",
                        "ts": attempt_ts,
                        "tokensUsed": tokens_used,
                    }));
                    req.status = "completed".to_string();
                    req.result = Some(text.clone());
                    req.completed_at = Some(chrono::Utc::now().to_rfc3339());

                    let completed = req.clone();
                    let req_id = id.to_string();
                    drop(state); // release lock before moving

                    let mut state2 = self.state.write().await;
                    state2.queue.retain(|r| r.id != req_id);
                    state2.completed.push(completed.clone());
                    drop(state2);

                    info!("brain: {} completed ({} tokens)", req_id, tokens_used);

                    // Fire callback if set
                    if let Some(url) = callback_url {
                        let cb_body = json!({
                            "requestId": req_id,
                            "result": text,
                            "completedAt": completed.completed_at,
                            "metadata": completed.metadata,
                        });
                        let c = client.clone();
                        tokio::spawn(async move {
                            let _ = c.post(&url).json(&cb_body).send().await;
                        });
                    }
                }
                Err(e) => {
                    req.attempts.push(json!({
                        "model": "tokenhub",
                        "ts": attempt_ts,
                        "error": e.to_string(),
                    }));
                    req.status = "pending".to_string(); // retry next tick
                    warn!("brain: {} tokenhub error: {} — will retry", id, e);
                }
            }
        }
    }

    async fn call_model(
        &self,
        client: &reqwest::Client,
        messages: &[Value],
        max_tokens: u32,
    ) -> Result<(String, u32), String> {
        for model in &self.models {
            match self
                .call_model_once(client, model, messages, max_tokens)
                .await
            {
                Ok((text, tokens)) if !text.is_empty() => return Ok((text, tokens)),
                Ok(_) => warn!("brain: empty response from {}", model),
                Err(e) => warn!("brain: {} failed: {} — trying next", model, e),
            }
        }
        Err("all models failed".to_string())
    }

    async fn call_model_once(
        &self,
        client: &reqwest::Client,
        model: &str,
        messages: &[Value],
        max_tokens: u32,
    ) -> Result<(String, u32), String> {
        let resp = client
            .post(format!("{}/v1/chat/completions", self.tokenhub_url))
            .bearer_auth(&self.tokenhub_key)
            .json(&json!({
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
            }))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {}: {}", status, &body[..body.len().min(200)]));
        }

        let data: Value = resp.json().await.map_err(|e| e.to_string())?;
        let msg = &data["choices"][0]["message"];
        let text = msg["content"]
            .as_str()
            .or_else(|| msg["reasoning"].as_str())
            .unwrap_or("")
            .to_string();
        let tokens = data["usage"]["total_tokens"].as_u64().unwrap_or(0) as u32;
        Ok((text, tokens))
    }
}

fn priority_order(p: &str) -> u8 {
    match p {
        "high" => 0,
        "low" => 2,
        _ => 1,
    }
}

// ── Background worker ─────────────────────────────────────────────────────

pub async fn run_brain_worker(brain: Arc<BrainQueue>, client: reqwest::Client) {
    brain.load().await;
    info!("brain: worker started (tick={}ms)", brain.tick_ms);
    let tick_dur = std::time::Duration::from_millis(brain.tick_ms);
    loop {
        // Wait for either a tick interval or a notify (new request enqueued)
        tokio::select! {
            _ = tokio::time::sleep(tick_dur) => {},
            _ = brain.notify.notified() => {},
        }
        brain.tick(&client).await;
    }
}
