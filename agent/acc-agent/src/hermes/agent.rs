use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::config::Config;
use crate::session_registry;
use acc_client::Client;
use acc_model::{HeartbeatRequest, Task, TaskStatus, TaskType};

use super::conversation::ConversationHistory;
use super::provider::LlmProvider;
use super::tool::ToolRegistry;

const MAX_ITERATIONS: u32 = 120;
const MAX_TOKENS: u32 = 8192;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const CLAUDE_ONLY_TAGS: &[&str] = &["claude", "claude_cli"];
const CLI_EXECUTORS: &[&str] = &["claude_cli", "codex_cli", "cursor_cli", "opencode"];

const SYSTEM_PROMPT_BASE: &str = "\
You are a capable AI assistant executing a task on a remote machine. \
Work methodically, verify your work, and be concise. \
When you have completed the task, summarize what you did in a final message.";

pub struct HermesAgent {
    cfg: Config,
    client: Client,
    http: reqwest::Client,
    provider: Box<dyn LlmProvider>,
    tools: ToolRegistry,
    shutdown: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
enum KeepaliveTarget {
    QueueItem(String),
    Task(String),
}

impl HermesAgent {
    pub fn new(
        cfg: Config,
        client: Client,
        provider: Box<dyn LlmProvider>,
        tools: ToolRegistry,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build shared HTTP client for HermesAgent");
        let shutdown = Arc::new(AtomicBool::new(false));
        let sd = shutdown.clone();
        tokio::spawn(async move {
            if let Ok(mut sig) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                sig.recv().await;
                sd.store(true, Ordering::SeqCst);
            }
        });
        Self {
            cfg,
            client,
            http,
            provider,
            tools,
            shutdown,
        }
    }

    pub async fn run_queue_item(&self, item_id: String, query: String) {
        self.register_capabilities().await;
        self.log(&format!(
            "starting item={item_id} query_len={}",
            query.len()
        ));

        if !self.claim_queue_item(&item_id).await {
            self.log(&format!("claim rejected for {item_id}"));
            return;
        }

        let workspace_query = if let Ok(ws) = std::env::var("TASK_WORKSPACE_LOCAL") {
            format!(
                "Your task workspace is: {ws}\n\
                     Work only within this directory. \
                     Do NOT run git commit or git push.\n\n{query}"
            )
        } else {
            query
        };

        let (ok, output) = self
            .run_conversation(
                Some(item_id.clone()),
                Some(KeepaliveTarget::QueueItem(item_id.clone())),
                workspace_query,
            )
            .await;
        if ok {
            self.post_queue_complete(&item_id, &output).await;
        } else {
            self.post_queue_fail(&item_id, &output).await;
        }
    }

    pub async fn run_task(&self, task_id: String, query: String) {
        self.register_capabilities().await;
        let query = if query.is_empty() {
            match self.client.tasks().get(&task_id).await {
                Ok(task) => self.task_query(&task),
                Err(_) => query,
            }
        } else {
            query
        };
        self.log(&format!(
            "starting task={task_id} query_len={}",
            query.len()
        ));

        if !self.claim_task(&task_id).await {
            self.log(&format!("claim rejected for {task_id}"));
            return;
        }

        let workspace_query = self.workspace_query(query);
        let (ok, output) = self
            .run_conversation(
                Some(task_id.clone()),
                Some(KeepaliveTarget::Task(task_id.clone())),
                workspace_query,
            )
            .await;
        if ok {
            self.post_task_complete(&task_id, &output).await;
        } else {
            self.unclaim_task(&task_id).await;
        }
    }

    pub async fn run_query(&self, query: String) {
        self.register_capabilities().await;
        self.log(&format!("running ad-hoc query len={}", query.len()));
        let (_, output) = self.run_conversation(None, None, query).await;
        println!("{output}");
    }

    pub async fn poll_tasks(&self) {
        self.register_capabilities().await;
        self.log(&format!(
            "starting task poll (agent={}, hub={})",
            self.cfg.agent_name, self.cfg.acc_url
        ));
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                self.log("shutting down (SIGTERM)");
                break;
            }
            if let Some(task) = self.fetch_task().await {
                let id = task.id.clone();
                if !self.claim_task(&id).await {
                    self.log(&format!("claim rejected for {id}"));
                    continue;
                }
                let query = self.workspace_query(self.task_query(&task));
                let (ok, output) = self
                    .run_conversation(
                        Some(id.clone()),
                        Some(KeepaliveTarget::Task(id.clone())),
                        query,
                    )
                    .await;
                if ok {
                    self.post_task_complete(&id, &output).await;
                } else {
                    self.log(&format!("task {id} failed: {output}"));
                    self.unclaim_task(&id).await;
                }
            } else {
                sleep(POLL_INTERVAL).await;
            }
        }
    }

    pub async fn poll_queue_legacy(&self) {
        self.register_capabilities().await;
        self.log(&format!(
            "starting legacy queue poll (agent={}, hub={})",
            self.cfg.agent_name, self.cfg.acc_url
        ));
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                self.log("shutting down (SIGTERM)");
                break;
            }
            if let Some(item) = self.fetch_queue_item().await {
                let id = item["id"].as_str().unwrap_or("").to_string();
                let query = format!(
                    "{}\n\n{}",
                    item["title"].as_str().unwrap_or(""),
                    item["description"].as_str().unwrap_or("")
                );
                self.run_queue_item(id, query).await;
            } else {
                sleep(POLL_INTERVAL).await;
            }
        }
    }

    async fn run_conversation(
        &self,
        item_id: Option<String>,
        keepalive_target: Option<KeepaliveTarget>,
        query: String,
    ) -> (bool, String) {
        let system = self.system_prompt();

        // Attempt to resume from stored turns if we have a task ID.
        let mut history = if let Some(ref id) = item_id {
            let stored_turns = self.load_turns(id).await;
            if !stored_turns.is_empty() {
                self.log(&format!(
                    "resuming from {} stored turns",
                    stored_turns.len()
                ));
                ConversationHistory::from_turns(&stored_turns)
            } else {
                let mut h = ConversationHistory::new();
                h.push_user_text(&query);
                h
            }
        } else {
            let mut h = ConversationHistory::new();
            h.push_user_text(&query);
            h
        };

        let tools_api = self.tools.to_api_format();

        let (ka_stop, mut ka_rx) = tokio::sync::oneshot::channel::<()>();
        {
            let cfg = self.cfg.clone();
            let client = self.client.clone();
            let keepalive_target2 = keepalive_target.clone();
            let tool_names = self.tools.names().join(", ");
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(KEEPALIVE_INTERVAL);
                interval.tick().await;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let note = format!("hermes-rust running (tools: {tool_names})");
                            post_heartbeat(&cfg, &client, &note).await;
                            if let Some(target) = keepalive_target2.as_ref() {
                                match target {
                                    KeepaliveTarget::QueueItem(id) => post_queue_keepalive(&cfg, &client, id, &note).await,
                                    KeepaliveTarget::Task(id) => post_task_keepalive(&cfg, &client, id, &note).await,
                                }
                            }
                        }
                        _ = &mut ka_rx => break,
                    }
                }
            });
        }

        let mut final_output = String::new();
        let mut success = false;

        for iteration in 1..=MAX_ITERATIONS {
            if self.shutdown.load(Ordering::SeqCst) {
                final_output = "interrupted by SIGTERM".to_string();
                break;
            }

            self.log(&format!(
                "iteration {iteration}/{MAX_ITERATIONS} tokens={}+{}",
                history.input_tokens, history.output_tokens
            ));

            let resp = match self
                .provider
                .complete(&system, &history.messages, &tools_api, MAX_TOKENS)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    self.log(&format!("provider error: {e}"));
                    final_output = format!("LLM error: {e}");
                    break;
                }
            };

            history.input_tokens += resp.input_tokens;
            history.output_tokens += resp.output_tokens;

            for block in &resp.content {
                if block["type"] == "text" {
                    if let Some(t) = block["text"].as_str() {
                        final_output = t.to_string();
                    }
                }
            }

            history.push_assistant_content(resp.content.clone());
            if let Some(ref id) = item_id {
                let assistant_content = json!(resp.content);
                self.save_turn(
                    id,
                    history.messages.len() as i64 - 1,
                    "assistant",
                    &assistant_content,
                    resp.input_tokens,
                    resp.output_tokens,
                    &resp.stop_reason,
                )
                .await;
            }

            match resp.stop_reason.as_str() {
                "end_turn" => {
                    self.log(&format!(
                        "completed at iteration {iteration}, total_tokens={}",
                        history.total_tokens()
                    ));
                    success = true;
                    break;
                }
                "tool_use" => {
                    let tool_results = self.execute_tools(&resp.content).await;
                    history.push_tool_results(tool_results.clone());
                    if let Some(ref id) = item_id {
                        let results_content = json!(tool_results);
                        self.save_turn(
                            id,
                            history.messages.len() as i64 - 1,
                            "user",
                            &results_content,
                            0,
                            0,
                            "tool_results",
                        )
                        .await;
                    }
                }
                "max_tokens" => {
                    self.log(&format!("token budget exhausted at iteration {iteration}"));
                    final_output = format!(
                        "Token budget exhausted after {iteration} iterations. Last output: {}",
                        &final_output[..final_output.len().min(500)]
                    );
                    break;
                }
                reason => {
                    self.log(&format!("unexpected stop reason: {reason}"));
                    success = true;
                    break;
                }
            }
        }

        let _ = ka_stop.send(());
        (success, final_output)
    }

    fn workspace_query(&self, query: String) -> String {
        if let Ok(ws) = std::env::var("TASK_WORKSPACE_LOCAL") {
            format!(
                "Your task workspace is: {ws}\n\
                 Work only within this directory. \
                 Do NOT run git commit or git push.\n\n{query}"
            )
        } else {
            query
        }
    }

    /// One conversational turn for the gateway. Appends the user message to the provided
    /// history, runs the LLM loop (with tool use), appends the assistant reply, and returns
    /// the final text. The caller owns the history and persists it between turns.
    pub(crate) async fn run_gateway_turn(
        &self,
        history: &mut ConversationHistory,
        message: &str,
        system: &str,
    ) -> String {
        history.push_user_text(message);
        let tools_api = self.tools.to_api_format();
        let mut final_output = String::new();

        for iteration in 1..=MAX_ITERATIONS {
            let resp = match self
                .provider
                .complete(system, &history.messages, &tools_api, MAX_TOKENS)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("[hermes-gateway] LLM error at iteration {iteration}: {e}");
                    return format!("I encountered an error: {e}");
                }
            };

            history.input_tokens += resp.input_tokens;
            history.output_tokens += resp.output_tokens;

            for block in &resp.content {
                if block["type"] == "text" {
                    if let Some(t) = block["text"].as_str() {
                        final_output = t.to_string();
                    }
                }
            }

            history.push_assistant_content(resp.content.clone());

            match resp.stop_reason.as_str() {
                "end_turn" => break,
                "tool_use" => {
                    let tool_results = self.execute_tools(&resp.content).await;
                    history.push_tool_results(tool_results);
                }
                "max_tokens" => {
                    return format!(
                        "I've reached my token limit. Here's what I have so far: {}",
                        &final_output[..final_output.len().min(1000)]
                    );
                }
                reason => {
                    tracing::debug!("[hermes-gateway] stop: {reason}");
                    break;
                }
            }
        }

        final_output
    }

    pub(crate) async fn register_capabilities(&self) {
        let caps = self.tools.names();
        let url = format!(
            "{}/api/agents/{}/capabilities",
            self.cfg.acc_url, self.cfg.agent_name
        );
        let body = serde_json::json!({"capabilities": caps});
        let _ = self
            .http
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.cfg.acc_token))
            .json(&body)
            .send()
            .await;
        self.log(&format!("registered capabilities: {}", caps.join(", ")));
    }

    pub(crate) async fn load_turns(&self, task_id: &str) -> Vec<Value> {
        let url = format!("{}/api/tasks/{}/turns", self.cfg.acc_url, task_id);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.cfg.acc_token))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["turns"].as_array().cloned())
                .unwrap_or_default(),
            _ => vec![],
        }
    }

    pub(crate) async fn save_turn(
        &self,
        task_id: &str,
        turn_index: i64,
        role: &str,
        content: &Value,
        input_tokens: u32,
        output_tokens: u32,
        stop_reason: &str,
    ) {
        let url = format!("{}/api/tasks/{}/turns", self.cfg.acc_url, task_id);
        let body = serde_json::json!({
            "turn_index":    turn_index,
            "role":          role,
            "content":       content,
            "input_tokens":  input_tokens,
            "output_tokens": output_tokens,
            "stop_reason":   stop_reason,
        });
        match self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.cfg.acc_token))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if !resp.status().is_success() => {
                let status = resp.status().as_u16();
                tracing::warn!("save_turn: task={task_id} idx={turn_index} HTTP {status}");
            }
            Err(e) => {
                tracing::warn!("save_turn: task={task_id} idx={turn_index} error: {e}");
            }
            Ok(_) => {}
        }
    }

    async fn execute_tools(&self, content: &[Value]) -> Vec<Value> {
        use futures_util::future::join_all;

        let futs: Vec<_> = content
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|block| {
                let id = block["id"].as_str().unwrap_or("").to_string();
                let name = block["name"].as_str().unwrap_or("").to_string();
                let input = block["input"].clone();
                self.run_one_tool(id, name, input)
            })
            .collect();

        join_all(futs).await
    }

    async fn run_one_tool(&self, tool_use_id: String, tool_name: String, input: Value) -> Value {
        self.log(&format!(
            "tool call: {tool_name}({})",
            serde_json::to_string(&input).unwrap_or_default()
        ));

        let (content_str, is_error) = match self.tools.get(&tool_name) {
            Some(tool) => match tool.execute(input).await {
                Ok(out) => (out, false),
                Err(e) => (e, true),
            },
            None => (format!("unknown tool: {tool_name}"), true),
        };

        // Cap output to ~16k chars to stay within context budget
        let truncated = &content_str[..content_str.len().min(16_384)];
        json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": truncated,
            "is_error": is_error,
        })
    }

    async fn fetch_queue_item(&self) -> Option<Value> {
        let items = self.client.queue().list().await.ok()?;
        for item in items {
            let raw = serde_json::to_value(&item).ok()?;
            if raw["status"].as_str() != Some("pending") {
                continue;
            }
            let assignee = raw["assignee"].as_str().unwrap_or("");
            if !assignee.is_empty() && assignee != "all" && assignee != self.cfg.agent_name.as_str()
            {
                continue;
            }
            let preferred = raw["preferred_executor"].as_str().unwrap_or("");
            let tags: Vec<&str> = raw["tags"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let is_claude_only =
                preferred == "claude_cli" || tags.iter().any(|t| CLAUDE_ONLY_TAGS.contains(t));
            if !is_claude_only {
                return Some(raw);
            }
        }
        None
    }

    async fn fetch_task(&self) -> Option<Task> {
        for task_type in [
            TaskType::Work,
            TaskType::Feature,
            TaskType::Bug,
            TaskType::Task,
        ] {
            let tasks = self
                .client
                .tasks()
                .list()
                .status(TaskStatus::Open)
                .task_type(task_type)
                .limit(25)
                .send()
                .await
                .ok()?;
            if let Some(task) = tasks.into_iter().find(|task| self.is_hermes_task(task)) {
                return Some(task);
            }
        }
        None
    }

    fn is_hermes_task(&self, task: &Task) -> bool {
        if task.review_of.is_some() {
            return false;
        }
        if matches!(
            task.task_type,
            TaskType::Review
                | TaskType::Idea
                | TaskType::Discovery
                | TaskType::PhaseCommit
                | TaskType::Epic
                | TaskType::Unknown
        ) {
            return false;
        }
        if task
            .assigned_agent
            .as_deref()
            .is_some_and(|name| name != self.cfg.agent_name)
        {
            return false;
        }
        if task
            .preferred_agent
            .as_deref()
            .is_some_and(|name| name != self.cfg.agent_name)
        {
            return false;
        }
        if let Some(exec) = task.preferred_executor.as_deref() {
            if CLI_EXECUTORS.contains(&exec) {
                return false;
            }
        }
        if !task.required_executors.is_empty()
            && !task
                .required_executors
                .iter()
                .any(|req| req == "hermes" || req == "llm")
        {
            return false;
        }
        true
    }

    fn task_query(&self, task: &Task) -> String {
        format!("{}\n\n{}", task.title, task.description)
    }

    async fn claim_queue_item(&self, item_id: &str) -> bool {
        self.client
            .items()
            .claim(item_id, &self.cfg.agent_name, Some("hermes-rust claiming"))
            .await
            .is_ok()
    }

    async fn claim_task(&self, task_id: &str) -> bool {
        self.client
            .tasks()
            .claim(task_id, &self.cfg.agent_name)
            .await
            .is_ok()
    }

    async fn post_queue_complete(&self, item_id: &str, result: &str) {
        let truncated = &result[..result.len().min(4000)];
        let _ = self
            .client
            .items()
            .complete(
                item_id,
                &self.cfg.agent_name,
                Some(truncated),
                Some(truncated),
            )
            .await;
    }

    async fn post_queue_fail(&self, item_id: &str, reason: &str) {
        let truncated = &reason[..reason.len().min(2000)];
        let _ = self
            .client
            .items()
            .fail(item_id, &self.cfg.agent_name, truncated)
            .await;
    }

    async fn post_task_complete(&self, task_id: &str, result: &str) {
        let truncated = &result[..result.len().min(4000)];
        let _ = self
            .client
            .tasks()
            .complete(task_id, Some(&self.cfg.agent_name), Some(truncated))
            .await;
    }

    async fn unclaim_task(&self, task_id: &str) {
        let _ = self
            .client
            .tasks()
            .unclaim(task_id, Some(&self.cfg.agent_name))
            .await;
    }

    fn system_prompt(&self) -> String {
        let tool_names = self.tools.names().join(", ");
        format!(
            "{SYSTEM_PROMPT_BASE}\n\nAgent: {}\nAvailable tools: {tool_names}",
            self.cfg.agent_name
        )
    }

    fn log(&self, msg: &str) {
        tracing::info!(component = "hermes-rust", agent = %self.cfg.agent_name, "{msg}");
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let line = format!("[{ts}] [{}] [hermes-rust] {msg}", self.cfg.agent_name);
        eprintln!("{line}");
        let log_path = self.cfg.log_file("hermes-rust");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }
}

async fn post_heartbeat(cfg: &Config, client: &Client, note: &str) {
    let truncated = &note[..note.len().min(200)];
    let req = HeartbeatRequest {
        ts: Some(chrono::Utc::now()),
        status: Some("ok".into()),
        note: Some(truncated.into()),
        host: Some(cfg.host.clone()),
        ssh_user: Some(cfg.ssh_user.clone()),
        ssh_host: Some(cfg.ssh_host.clone()),
        ssh_port: Some(cfg.ssh_port as u64),
        tasks_in_flight: None,
        estimated_free_slots: None,
        free_session_slots: None,
        max_sessions: None,
        session_spawn_denied_reason: None,
        executors: vec![],
        sessions: vec![],
    };
    let mut req = req;
    session_registry::augment_heartbeat(cfg, &mut req).await;
    let _ = client.items().heartbeat(&cfg.agent_name, &req).await;
}

async fn post_queue_keepalive(cfg: &Config, client: &Client, item_id: &str, note: &str) {
    let _ = client
        .items()
        .keepalive(item_id, &cfg.agent_name, Some(note))
        .await;
}

async fn post_task_keepalive(cfg: &Config, _client: &Client, task_id: &str, note: &str) {
    let url = format!("{}/api/tasks/{}/keepalive", cfg.acc_url, task_id);
    let body = serde_json::json!({
        "agent": cfg.agent_name,
        "extend_mins": 30,
        "note": note,
    });
    let _ = reqwest::Client::new()
        .put(&url)
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .send()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_mock::HubMock;
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::AtomicU32;

    fn test_cfg(url: &str) -> Config {
        Config {
            acc_dir: PathBuf::from("/tmp"),
            acc_url: url.to_string(),
            acc_token: "test-tok".to_string(),
            agent_name: "natasha".to_string(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: String::new(),
            ssh_user: "testuser".into(),
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
        }
    }

    fn build_client(cfg: &Config) -> Client {
        Client::new(&cfg.acc_url, &cfg.acc_token).expect("client")
    }

    // ── Test providers ────────────────────────────────────────────────────────

    /// Always returns a single text block with end_turn.
    struct EchoProvider {
        reply: String,
    }

    impl LlmProvider for EchoProvider {
        fn complete<'a>(
            &'a self,
            _system: &'a str,
            _messages: &'a [Value],
            _tools: &'a [Value],
            _max_tokens: u32,
        ) -> Pin<Box<dyn Future<Output = super::super::provider::ProviderResult> + Send + 'a>>
        {
            let reply = self.reply.clone();
            Box::pin(async move {
                Ok(super::super::provider::LlmResponse {
                    content: vec![json!({"type": "text", "text": reply})],
                    stop_reason: "end_turn".to_string(),
                    input_tokens: 10,
                    output_tokens: 5,
                })
            })
        }
    }

    /// First call returns a bash tool_use; second call returns end_turn.
    struct TwoStepProvider {
        step: Arc<AtomicU32>,
    }

    impl LlmProvider for TwoStepProvider {
        fn complete<'a>(
            &'a self,
            _system: &'a str,
            _messages: &'a [Value],
            _tools: &'a [Value],
            _max_tokens: u32,
        ) -> Pin<Box<dyn Future<Output = super::super::provider::ProviderResult> + Send + 'a>>
        {
            let step = self.step.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if step == 0 {
                    Ok(super::super::provider::LlmResponse {
                        content: vec![json!({
                            "type": "tool_use",
                            "id": "call-1",
                            "name": "bash",
                            "input": {"command": "echo tool_ran"}
                        })],
                        stop_reason: "tool_use".to_string(),
                        input_tokens: 10,
                        output_tokens: 5,
                    })
                } else {
                    Ok(super::super::provider::LlmResponse {
                        content: vec![
                            json!({"type": "text", "text": "tool executed successfully"}),
                        ],
                        stop_reason: "end_turn".to_string(),
                        input_tokens: 20,
                        output_tokens: 10,
                    })
                }
            })
        }
    }

    /// Always returns max_tokens stop reason.
    struct MaxTokensProvider;

    impl LlmProvider for MaxTokensProvider {
        fn complete<'a>(
            &'a self,
            _system: &'a str,
            _messages: &'a [Value],
            _tools: &'a [Value],
            _max_tokens: u32,
        ) -> Pin<Box<dyn Future<Output = super::super::provider::ProviderResult> + Send + 'a>>
        {
            Box::pin(async move {
                Ok(super::super::provider::LlmResponse {
                    content: vec![json!({"type": "text", "text": "partial work done"})],
                    stop_reason: "max_tokens".to_string(),
                    input_tokens: 8000,
                    output_tokens: 8192,
                })
            })
        }
    }

    fn make_agent(url: &str) -> HermesAgent {
        let cfg = test_cfg(url);
        let client = build_client(&cfg);
        HermesAgent::new(
            cfg,
            client,
            Box::new(EchoProvider {
                reply: "task done".to_string(),
            }),
            ToolRegistry::default_tools(),
        )
    }

    // ── run_conversation tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn run_conversation_returns_success_and_text() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(EchoProvider {
                reply: "task done".to_string(),
            }),
            ToolRegistry::default_tools(),
        );
        let (ok, output) = agent
            .run_conversation(None, None, "do the thing".to_string())
            .await;
        assert!(
            ok,
            "EchoProvider returns end_turn so conversation must succeed"
        );
        assert_eq!(
            output, "task done",
            "output must be the provider's text reply"
        );
    }

    #[tokio::test]
    async fn run_conversation_tool_use_executes_bash_and_continues() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(TwoStepProvider {
                step: Arc::new(AtomicU32::new(0)),
            }),
            ToolRegistry::default_tools(),
        );
        // Step 1: provider returns tool_use(bash, echo tool_ran)
        // Step 2: agent executes bash, adds result to history, calls provider again
        // Step 3: provider returns end_turn with "tool executed successfully"
        let (ok, output) = agent
            .run_conversation(None, None, "run a tool".to_string())
            .await;
        assert!(ok, "should succeed after tool execution");
        assert_eq!(output, "tool executed successfully");
    }

    #[tokio::test]
    async fn run_conversation_max_tokens_returns_failure() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(MaxTokensProvider),
            ToolRegistry::default_tools(),
        );
        let (ok, output) = agent
            .run_conversation(None, None, "a query".to_string())
            .await;
        assert!(!ok, "max_tokens must signal failure");
        assert!(
            output.contains("Token budget exhausted"),
            "output must explain token exhaustion, got: {output:?}"
        );
    }

    // ── fetch_queue_item tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_queue_item_returns_none_on_empty_queue() {
        let mock = HubMock::new().await;
        let agent = make_agent(&mock.url);
        assert!(agent.fetch_queue_item().await.is_none());
    }

    #[tokio::test]
    async fn fetch_queue_item_skips_claude_only_tag() {
        let mock = HubMock::with_queue(vec![json!({
            "id": "wq-c1", "status": "pending", "assignee": "all",
            "tags": ["claude_cli"], "preferred_executor": ""
        })])
        .await;
        assert!(make_agent(&mock.url).fetch_queue_item().await.is_none());
    }

    #[tokio::test]
    async fn fetch_queue_item_skips_claude_only_preferred_executor() {
        let mock = HubMock::with_queue(vec![json!({
            "id": "wq-c2", "status": "pending", "assignee": "all",
            "tags": [], "preferred_executor": "claude_cli"
        })])
        .await;
        assert!(make_agent(&mock.url).fetch_queue_item().await.is_none());
    }

    #[tokio::test]
    async fn fetch_queue_item_returns_eligible_item() {
        let mock = HubMock::with_queue(vec![json!({
            "id": "wq-ok", "status": "pending", "assignee": "all",
            "tags": ["gpu"], "preferred_executor": ""
        })])
        .await;
        let item = make_agent(&mock.url).fetch_queue_item().await;
        assert!(item.is_some());
        assert_eq!(item.unwrap()["id"], "wq-ok");
    }

    #[tokio::test]
    async fn fetch_task_returns_hermes_eligible_task() {
        let mock = HubMock::with_tasks(vec![
            json!({
                "id": "task-cli",
                "status": "open",
                "task_type": "work",
                "title": "CLI only",
                "description": "skip me",
                "preferred_executor": "claude_cli"
            }),
            json!({
                "id": "task-hermes",
                "status": "open",
                "task_type": "work",
                "title": "Hermes task",
                "description": "handle me",
                "preferred_executor": "hermes"
            }),
        ])
        .await;
        let task = make_agent(&mock.url)
            .fetch_task()
            .await
            .expect("eligible task");
        assert_eq!(task.id, "task-hermes");
    }

    #[tokio::test]
    async fn fetch_task_skips_task_assigned_to_other_agent() {
        let mock = HubMock::with_tasks(vec![json!({
            "id": "task-foreign",
            "status": "open",
            "task_type": "work",
            "title": "Assigned elsewhere",
            "description": "skip me",
            "assigned_agent": "boris"
        })])
        .await;
        assert!(make_agent(&mock.url).fetch_task().await.is_none());
    }

    #[tokio::test]
    async fn is_hermes_task_skips_cli_executor() {
        let agent = make_agent("http://example.invalid");
        let task = Task {
            id: "task-1".to_string(),
            project_id: String::new(),
            title: "CLI task".to_string(),
            description: String::new(),
            status: TaskStatus::Open,
            priority: 2,
            claimed_by: None,
            claimed_at: None,
            claim_expires_at: None,
            completed_at: None,
            completed_by: None,
            created_at: None,
            metadata: json!({}),
            preferred_executor: Some("claude_cli".to_string()),
            required_executors: vec![],
            preferred_agent: None,
            assigned_agent: None,
            assigned_session: None,
            outcome_id: None,
            workflow_role: None,
            finisher_agent: None,
            finisher_session: None,
            chain_id: None,
            task_type: TaskType::Work,
            review_of: None,
            phase: None,
            blocked_by: vec![],
            review_result: None,
        };
        assert!(!agent.is_hermes_task(&task));
    }

    #[tokio::test]
    async fn task_query_combines_title_and_description() {
        let agent = make_agent("http://example.invalid");
        let task = Task {
            id: "task-2".to_string(),
            project_id: String::new(),
            title: "Title".to_string(),
            description: "Body".to_string(),
            status: TaskStatus::Open,
            priority: 2,
            claimed_by: None,
            claimed_at: None,
            claim_expires_at: None,
            completed_at: None,
            completed_by: None,
            created_at: None,
            metadata: json!({}),
            preferred_executor: None,
            required_executors: vec![],
            preferred_agent: None,
            assigned_agent: None,
            assigned_session: None,
            outcome_id: None,
            workflow_role: None,
            finisher_agent: None,
            finisher_session: None,
            chain_id: None,
            task_type: TaskType::Work,
            review_of: None,
            phase: None,
            blocked_by: vec![],
            review_result: None,
        };
        assert_eq!(agent.task_query(&task), "Title\n\nBody");
    }

    // ── claim tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn claim_succeeds_against_mock() {
        let mock = HubMock::new().await;
        assert!(make_agent(&mock.url).claim_queue_item("wq-test").await);
    }

    #[tokio::test]
    async fn claim_conflict_returns_false() {
        use crate::hub_mock::HubState;
        let mock = HubMock::with_state(HubState {
            item_claim_status: 409,
            ..Default::default()
        })
        .await;
        assert!(!make_agent(&mock.url).claim_queue_item("wq-clash").await);
    }

    // ── tool advertisement ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn tool_names_advertised_in_registry() {
        let mock = HubMock::new().await;
        let agent = make_agent(&mock.url);
        let names = agent.tools.names();
        assert!(names.contains(&"bash".to_string()));
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"web_fetch".to_string()));
    }

    // ── run_item tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn run_queue_item_completes_on_success() {
        use crate::hub_mock::HubState;
        let mock = HubMock::with_state(HubState {
            queue_items: vec![json!({
                "id": "wq-item-1", "status": "pending", "assignee": "all",
                "tags": [], "preferred_executor": ""
            })],
            ..Default::default()
        })
        .await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(EchoProvider {
                reply: "done".to_string(),
            }),
            ToolRegistry::default_tools(),
        );
        agent
            .run_queue_item("wq-item-1".to_string(), "test task".to_string())
            .await;
        let log = mock.state.read().await.call_log.lock().await.clone();
        assert!(
            log.iter()
                .any(|e| e.contains("/api/item/wq-item-1/complete")),
            "complete must be called after successful run; log={log:?}"
        );
    }

    #[tokio::test]
    async fn run_queue_item_fails_on_max_tokens() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(MaxTokensProvider),
            ToolRegistry::default_tools(),
        );
        agent
            .run_queue_item("wq-item-2".to_string(), "another task".to_string())
            .await;
        let log = mock.state.read().await.call_log.lock().await.clone();
        assert!(
            log.iter().any(|e| e.contains("/api/item/wq-item-2/fail")),
            "fail must be called when max_tokens hit; log={log:?}"
        );
    }

    #[tokio::test]
    async fn run_task_completes_on_success() {
        let mock = HubMock::with_tasks(vec![json!({
            "id": "task-1",
            "status": "open",
            "task_type": "work",
            "title": "Hermes task",
            "description": "complete me",
            "preferred_executor": "hermes"
        })])
        .await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(EchoProvider {
                reply: "done".to_string(),
            }),
            ToolRegistry::default_tools(),
        );
        agent
            .run_task("task-1".to_string(), "test task".to_string())
            .await;
        let log = mock.state.read().await.call_log.lock().await.clone();
        assert!(
            log.iter().any(|e| e.contains("/api/tasks/task-1/complete")),
            "task complete must be called after successful run; log={log:?}"
        );
    }

    #[tokio::test]
    async fn run_task_unclaims_on_failure() {
        let mock = HubMock::with_tasks(vec![json!({
            "id": "task-2",
            "status": "open",
            "task_type": "work",
            "title": "Hermes task",
            "description": "fail me",
            "preferred_executor": "hermes"
        })])
        .await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(MaxTokensProvider),
            ToolRegistry::default_tools(),
        );
        agent
            .run_task("task-2".to_string(), "another task".to_string())
            .await;
        let log = mock.state.read().await.call_log.lock().await.clone();
        assert!(
            log.iter().any(|e| e.contains("/api/tasks/task-2/unclaim")),
            "task unclaim must be called on failure; log={log:?}"
        );
    }

    // ── turns round-trip test ──────────────────────────────────────────────────

    #[tokio::test]
    async fn turns_save_and_load_round_trip() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(EchoProvider {
                reply: "done".to_string(),
            }),
            ToolRegistry::default_tools(),
        );
        // Save a turn
        agent
            .save_turn(
                "task-abc",
                0,
                "assistant",
                &serde_json::json!([{"type": "text", "text": "hello"}]),
                10,
                5,
                "end_turn",
            )
            .await;
        // Load it back
        let turns = agent.load_turns("task-abc").await;
        assert_eq!(turns.len(), 1, "should have one stored turn");
        assert_eq!(turns[0]["role"], "assistant");
    }

    // ── capability registration ─────────────────────────────────────────────────

    #[tokio::test]
    async fn register_capabilities_stores_to_hub() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg,
            client,
            Box::new(EchoProvider {
                reply: "ok".to_string(),
            }),
            ToolRegistry::default_tools(),
        );
        agent.register_capabilities().await;
        let caps = mock
            .state
            .read()
            .await
            .agent_capabilities
            .lock()
            .await
            .get("natasha")
            .cloned()
            .unwrap_or_default();
        assert!(
            caps.contains(&"bash".to_string()),
            "bash must be in registered capabilities"
        );
        assert!(
            caps.contains(&"web_fetch".to_string()),
            "web_fetch must be in registered capabilities"
        );
    }
}
