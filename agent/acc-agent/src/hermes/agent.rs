use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use serde_json::{json, Value};
use tokio::time::sleep;

use acc_client::Client;
use acc_model::HeartbeatRequest;
use crate::config::Config;

use super::conversation::ConversationHistory;
use super::provider::LlmProvider;
use super::tool::ToolRegistry;

const MAX_ITERATIONS: u32 = 120;
const MAX_TOKENS: u32 = 8192;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const CLAUDE_ONLY_TAGS: &[&str] = &["claude", "claude_cli"];

const SYSTEM_PROMPT_BASE: &str = "\
You are a capable AI assistant executing a task on a remote machine. \
Work methodically, verify your work, and be concise. \
When you have completed the task, summarize what you did in a final message.";

pub struct HermesAgent {
    cfg: Config,
    client: Client,
    provider: Box<dyn LlmProvider>,
    tools: ToolRegistry,
    shutdown: Arc<AtomicBool>,
}

impl HermesAgent {
    pub fn new(
        cfg: Config,
        client: Client,
        provider: Box<dyn LlmProvider>,
        tools: ToolRegistry,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let sd = shutdown.clone();
        tokio::spawn(async move {
            if let Ok(mut sig) = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::terminate(),
            ) {
                sig.recv().await;
                sd.store(true, Ordering::SeqCst);
            }
        });
        Self { cfg, client, provider, tools, shutdown }
    }

    pub async fn run_item(&self, item_id: String, query: String) {
        self.register_capabilities().await;
        self.log(&format!("starting item={item_id} query_len={}", query.len()));

        if !self.claim(&item_id).await {
            self.log(&format!("claim rejected for {item_id}"));
            return;
        }

        let workspace_query =
            if let Ok(ws) = std::env::var("TASK_WORKSPACE_LOCAL") {
                format!(
                    "Your task workspace is: {ws}\n\
                     Work only within this directory. \
                     Do NOT run git commit or git push.\n\n{query}"
                )
            } else {
                query
            };

        let (ok, output) =
            self.run_conversation(Some(item_id.clone()), workspace_query).await;
        if ok {
            self.post_complete(&item_id, &output).await;
        } else {
            self.post_fail(&item_id, &output).await;
        }
    }

    pub async fn run_query(&self, query: String) {
        self.register_capabilities().await;
        self.log(&format!("running ad-hoc query len={}", query.len()));
        let (_, output) = self.run_conversation(None, query).await;
        println!("{output}");
    }

    pub async fn poll_queue(&self) {
        self.register_capabilities().await;
        self.log(&format!(
            "starting queue poll (agent={}, hub={})",
            self.cfg.agent_name, self.cfg.acc_url
        ));
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                self.log("shutting down (SIGTERM)");
                break;
            }
            if let Some(item) = self.fetch_item().await {
                let id = item["id"].as_str().unwrap_or("").to_string();
                let query = format!(
                    "{}\n\n{}",
                    item["title"].as_str().unwrap_or(""),
                    item["description"].as_str().unwrap_or("")
                );
                self.run_item(id, query).await;
            } else {
                sleep(POLL_INTERVAL).await;
            }
        }
    }

    pub(crate) async fn run_conversation(
        &self,
        item_id: Option<String>,
        query: String,
    ) -> (bool, String) {
        let system = self.system_prompt();

        // Attempt to resume from stored turns if we have a task ID.
        let mut history = if let Some(ref id) = item_id {
            let stored_turns = self.load_turns(id).await;
            if !stored_turns.is_empty() {
                self.log(&format!("resuming from {} stored turns", stored_turns.len()));
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
            let item_id2 = item_id.clone();
            let tool_names = self.tools.names().join(", ");
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(KEEPALIVE_INTERVAL);
                interval.tick().await;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            let note = format!("hermes-native running (tools: {tool_names})");
                            post_heartbeat(&cfg, &client, &note).await;
                            if let Some(ref id) = item_id2 {
                                post_keepalive(&cfg, &client, id, &note).await;
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
                ).await;
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
                        ).await;
                    }
                }
                "max_tokens" => {
                    self.log(&format!(
                        "token budget exhausted at iteration {iteration}"
                    ));
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

    async fn register_capabilities(&self) {
        let caps = self.tools.names();
        let url = format!(
            "{}/api/agents/{}/capabilities",
            self.cfg.acc_url, self.cfg.agent_name
        );
        let body = serde_json::json!({"capabilities": caps});
        let _ = reqwest::Client::new()
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.cfg.acc_token))
            .json(&body)
            .send()
            .await;
        self.log(&format!("registered capabilities: {}", caps.join(", ")));
    }

    async fn load_turns(&self, task_id: &str) -> Vec<Value> {
        let url = format!("{}/api/tasks/{}/turns", self.cfg.acc_url, task_id);
        let resp = reqwest::Client::new()
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.cfg.acc_token))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                r.json::<Value>().await
                    .ok()
                    .and_then(|v| v["turns"].as_array().cloned())
                    .unwrap_or_default()
            }
            _ => vec![],
        }
    }

    async fn save_turn(
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
        let _ = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.cfg.acc_token))
            .json(&body)
            .send()
            .await;
    }

    async fn execute_tools(&self, content: &[Value]) -> Vec<Value> {
        let mut results = Vec::new();
        for block in content {
            if block["type"] != "tool_use" {
                continue;
            }
            let tool_use_id = block["id"].as_str().unwrap_or("").to_string();
            let tool_name = block["name"].as_str().unwrap_or("");
            let input = block["input"].clone();

            self.log(&format!(
                "tool call: {tool_name}({})",
                serde_json::to_string(&input).unwrap_or_default()
            ));

            let (content_str, is_error) = match self.tools.get(tool_name) {
                Some(tool) => match tool.execute(input).await {
                    Ok(out) => (out, false),
                    Err(e) => (e, true),
                },
                None => (format!("unknown tool: {tool_name}"), true),
            };

            // Cap tool output sent back to LLM to avoid blowing context
            let truncated = &content_str[..content_str.len().min(100_000)];
            results.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": truncated,
                "is_error": is_error,
            }));
        }
        results
    }

    async fn fetch_item(&self) -> Option<Value> {
        let items = self.client.queue().list().await.ok()?;
        for item in items {
            let raw = serde_json::to_value(&item).ok()?;
            if raw["status"].as_str() != Some("pending") {
                continue;
            }
            let assignee = raw["assignee"].as_str().unwrap_or("");
            if !assignee.is_empty()
                && assignee != "all"
                && assignee != self.cfg.agent_name.as_str()
            {
                continue;
            }
            let preferred = raw["preferred_executor"].as_str().unwrap_or("");
            let tags: Vec<&str> = raw["tags"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let is_claude_only = preferred == "claude_cli"
                || tags.iter().any(|t| CLAUDE_ONLY_TAGS.contains(t));
            if !is_claude_only {
                return Some(raw);
            }
        }
        None
    }

    async fn claim(&self, item_id: &str) -> bool {
        self.client
            .items()
            .claim(item_id, &self.cfg.agent_name, Some("hermes-native claiming"))
            .await
            .is_ok()
    }

    async fn post_complete(&self, item_id: &str, result: &str) {
        let truncated = &result[..result.len().min(4000)];
        let _ = self
            .client
            .items()
            .complete(item_id, &self.cfg.agent_name, Some(truncated), Some(truncated))
            .await;
    }

    async fn post_fail(&self, item_id: &str, reason: &str) {
        let truncated = &reason[..reason.len().min(2000)];
        let _ = self
            .client
            .items()
            .fail(item_id, &self.cfg.agent_name, truncated)
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
        tracing::info!(component = "hermes-native", agent = %self.cfg.agent_name, "{msg}");
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let line = format!("[{ts}] [{}] [hermes-native] {msg}", self.cfg.agent_name);
        eprintln!("{line}");
        let log_path = self.cfg.log_file("hermes-native");
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
    };
    let _ = client.items().heartbeat(&cfg.agent_name, &req).await;
}

async fn post_keepalive(cfg: &Config, client: &Client, item_id: &str, note: &str) {
    let _ = client
        .items()
        .keepalive(item_id, &cfg.agent_name, Some(note))
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
                        content: vec![json!({"type": "text", "text": "tool executed successfully"})],
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
        HermesAgent::new(cfg, client, Box::new(EchoProvider { reply: "task done".to_string() }), ToolRegistry::default_tools())
    }

    // ── run_conversation tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn run_conversation_returns_success_and_text() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg, client,
            Box::new(EchoProvider { reply: "task done".to_string() }),
            ToolRegistry::default_tools(),
        );
        let (ok, output) = agent.run_conversation(None, "do the thing".to_string()).await;
        assert!(ok, "EchoProvider returns end_turn so conversation must succeed");
        assert_eq!(output, "task done", "output must be the provider's text reply");
    }

    #[tokio::test]
    async fn run_conversation_tool_use_executes_bash_and_continues() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg, client,
            Box::new(TwoStepProvider { step: Arc::new(AtomicU32::new(0)) }),
            ToolRegistry::default_tools(),
        );
        // Step 1: provider returns tool_use(bash, echo tool_ran)
        // Step 2: agent executes bash, adds result to history, calls provider again
        // Step 3: provider returns end_turn with "tool executed successfully"
        let (ok, output) = agent.run_conversation(None, "run a tool".to_string()).await;
        assert!(ok, "should succeed after tool execution");
        assert_eq!(output, "tool executed successfully");
    }

    #[tokio::test]
    async fn run_conversation_max_tokens_returns_failure() {
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = build_client(&cfg);
        let agent = HermesAgent::new(
            cfg, client,
            Box::new(MaxTokensProvider),
            ToolRegistry::default_tools(),
        );
        let (ok, output) = agent.run_conversation(None, "a query".to_string()).await;
        assert!(!ok, "max_tokens must signal failure");
        assert!(
            output.contains("Token budget exhausted"),
            "output must explain token exhaustion, got: {output:?}"
        );
    }

    // ── fetch_item tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_item_returns_none_on_empty_queue() {
        let mock = HubMock::new().await;
        let agent = make_agent(&mock.url);
        assert!(agent.fetch_item().await.is_none());
    }

    #[tokio::test]
    async fn fetch_item_skips_claude_only_tag() {
        let mock = HubMock::with_queue(vec![json!({
            "id": "wq-c1", "status": "pending", "assignee": "all",
            "tags": ["claude_cli"], "preferred_executor": ""
        })]).await;
        assert!(make_agent(&mock.url).fetch_item().await.is_none());
    }

    #[tokio::test]
    async fn fetch_item_skips_claude_only_preferred_executor() {
        let mock = HubMock::with_queue(vec![json!({
            "id": "wq-c2", "status": "pending", "assignee": "all",
            "tags": [], "preferred_executor": "claude_cli"
        })]).await;
        assert!(make_agent(&mock.url).fetch_item().await.is_none());
    }

    #[tokio::test]
    async fn fetch_item_returns_eligible_item() {
        let mock = HubMock::with_queue(vec![json!({
            "id": "wq-ok", "status": "pending", "assignee": "all",
            "tags": ["gpu"], "preferred_executor": ""
        })]).await;
        let item = make_agent(&mock.url).fetch_item().await;
        assert!(item.is_some());
        assert_eq!(item.unwrap()["id"], "wq-ok");
    }

    // ── claim tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn claim_succeeds_against_mock() {
        let mock = HubMock::new().await;
        assert!(make_agent(&mock.url).claim("wq-test").await);
    }

    #[tokio::test]
    async fn claim_conflict_returns_false() {
        use crate::hub_mock::HubState;
        let mock = HubMock::with_state(HubState { item_claim_status: 409, ..Default::default() }).await;
        assert!(!make_agent(&mock.url).claim("wq-clash").await);
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
}
