use std::future::Future;
use std::pin::Pin;
use serde_json::{json, Value};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct LlmResponse {
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

pub type ProviderResult = Result<LlmResponse, String>;

/// Object-safe LLM provider trait. Uses boxed futures so Box<dyn LlmProvider> works.
pub trait LlmProvider: Send + Sync {
    fn complete<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Value],
        tools: &'a [Value],
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = ProviderResult> + Send + 'a>>;
}

/// Anthropic Messages API provider.
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
        Self::with_base_url(api_key, model, base_url)
    }

    pub fn with_base_url(api_key: String, model: String, base_url: String) -> Self {
        let base_url = base_url.trim_end_matches('/').trim_end_matches("/v1").to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client for AnthropicProvider");
        Self { api_key, model, client, base_url }
    }
}

impl LlmProvider for AnthropicProvider {
    fn complete<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Value],
        tools: &'a [Value],
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = ProviderResult> + Send + 'a>> {
        Box::pin(async move {
            let mut body = json!({
                "model": self.model,
                "max_tokens": max_tokens,
                "system": system,
                "messages": messages,
            });
            if !tools.is_empty() {
                body["tools"] = json!(tools);
            }

            let resp = self
                .client
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("HTTP error: {e}"))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("API error {status}: {}", &text[..text.len().min(500)]));
            }

            let val: Value = resp
                .json()
                .await
                .map_err(|e| format!("JSON parse error: {e}"))?;

            let content = val["content"].as_array().cloned().unwrap_or_default();
            let stop_reason = val["stop_reason"]
                .as_str()
                .unwrap_or("end_turn")
                .to_string();
            let input_tokens =
                val["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
            let output_tokens =
                val["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;

            Ok(LlmResponse {
                content,
                stop_reason,
                input_tokens,
                output_tokens,
            })
        })
    }
}

/// OpenAI-compatible provider — works with Ollama, OpenRouter, any /v1/chat/completions endpoint.
/// Set HERMES_PROVIDER=openai (or any non-empty OPENAI_BASE_URL) to activate.
pub struct OpenAiProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String, model: String) -> Self {
        let base_url = std::env::var("OPENAI_BASE_URL")
            .or_else(|_| std::env::var("HERMES_BACKEND_URL"))
            .unwrap_or_else(|_| "https://api.openai.com".to_string());
        Self::with_base_url(api_key, model, base_url)
    }

    pub fn with_base_url(api_key: String, model: String, base_url: String) -> Self {
        let base_url = base_url.trim_end_matches('/').trim_end_matches("/v1").to_string();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client for OpenAiProvider");
        Self { api_key, model, client, base_url }
    }
}

impl LlmProvider for OpenAiProvider {
    fn complete<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Value],
        tools: &'a [Value],
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = ProviderResult> + Send + 'a>> {
        Box::pin(async move {
            // Translate Anthropic messages → OpenAI chat format
            let mut oai_messages = vec![json!({"role": "system", "content": system})];
            for msg in messages {
                let role = msg["role"].as_str().unwrap_or("user");
                let content = &msg["content"];
                // Content may be array (Anthropic) or string (simple)
                let oai_content: Value = if content.is_array() {
                    let parts = content.as_array().map(Vec::as_slice).unwrap_or_default();
                    // Collect text blocks; tool_use becomes tool_calls on assistant, tool_result becomes tool on user
                    let text_parts: Vec<&Value> = parts.iter().filter(|p| p["type"] == "text").collect();
                    if text_parts.len() == 1 {
                        text_parts[0]["text"].clone()
                    } else if text_parts.is_empty() {
                        // Handle tool results or tool_use — pass raw for now
                        json!(serde_json::to_string(content).unwrap_or_default())
                    } else {
                        json!(text_parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"))
                    }
                } else {
                    content.clone()
                };
                oai_messages.push(json!({"role": role, "content": oai_content}));
            }

            // Translate Anthropic tool format → OpenAI function format
            let oai_tools: Vec<Value> = tools.iter().map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t["name"],
                        "description": t["description"],
                        "parameters": t["input_schema"]
                    }
                })
            }).collect();

            let mut body = json!({
                "model": self.model,
                "max_tokens": max_tokens,
                "messages": oai_messages,
            });
            if !oai_tools.is_empty() {
                body["tools"] = json!(oai_tools);
            }

            let resp = self
                .client
                .post(format!("{}/v1/chat/completions", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("HTTP error: {e}"))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("API error {status}: {}", &text[..text.len().min(500)]));
            }

            let val: Value = resp
                .json()
                .await
                .map_err(|e| format!("JSON parse error: {e}"))?;

            let choice = &val["choices"][0];
            let msg = &choice["message"];
            let stop_reason = choice["finish_reason"].as_str().unwrap_or("stop").to_string();

            // Translate back to Anthropic content format
            let mut content_blocks: Vec<Value> = Vec::new();

            if let Some(text) = msg["content"].as_str() {
                if !text.is_empty() {
                    content_blocks.push(json!({"type": "text", "text": text}));
                }
            }

            // Translate tool_calls → Anthropic tool_use blocks
            if let Some(tool_calls) = msg["tool_calls"].as_array() {
                for tc in tool_calls {
                    let fn_name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                    content_blocks.push(json!({
                        "type": "tool_use",
                        "id": tc["id"].as_str().unwrap_or("call-0"),
                        "name": fn_name,
                        "input": args
                    }));
                }
            }

            // Normalize stop_reason to Anthropic format
            let anthropic_stop = match stop_reason.as_str() {
                "tool_calls" => "tool_use",
                "length" => "max_tokens",
                "stop" | "" => "end_turn",
                other => other,
            }.to_string();

            let input_tokens = val["usage"]["prompt_tokens"].as_u64().unwrap_or(0) as u32;
            let output_tokens = val["usage"]["completion_tokens"].as_u64().unwrap_or(0) as u32;

            Ok(LlmResponse {
                content: content_blocks,
                stop_reason: anthropic_stop,
                input_tokens,
                output_tokens,
            })
        })
    }
}

/// Select provider based on environment variables.
/// - HERMES_PROVIDER=openai → OpenAiProvider (also triggered by OPENAI_BASE_URL or HERMES_BACKEND_URL)
/// - default → AnthropicProvider
// ── Provider configuration ────────────────────────────────────────────────────

/// One entry in an ordered provider list. Priority is ascending (0 = try first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// "anthropic" | "openai" | "openai-compat"
    #[serde(rename = "type")]
    pub provider_type: String,
    /// Base URL (required for openai-compat; optional for anthropic/openai).
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub model: String,
    pub label: Option<String>,
    #[serde(default)]
    pub priority: u8,
}

impl ProviderEntry {
    fn build(&self) -> Box<dyn LlmProvider> {
        let key = self.api_key.clone().unwrap_or_default();
        match self.provider_type.as_str() {
            "anthropic" => {
                let base = self.url.clone()
                    .unwrap_or_else(|| "https://api.anthropic.com".to_string());
                Box::new(AnthropicProvider::with_base_url(key, self.model.clone(), base))
            }
            _ => {
                // "openai" | "openai-compat" — anything with a /v1/chat/completions endpoint
                let base = self.url.clone()
                    .unwrap_or_else(|| "https://api.openai.com".to_string());
                Box::new(OpenAiProvider::with_base_url(key, self.model.clone(), base))
            }
        }
    }
}

// ── ProviderChain ─────────────────────────────────────────────────────────────

/// Wraps multiple providers in priority order. On each completion attempt, tries
/// providers sequentially and returns the first successful non-empty response.
/// Falls through on any error or empty content.
pub struct ProviderChain {
    providers: Vec<(String, Box<dyn LlmProvider>)>, // (label, provider)
}

impl ProviderChain {
    pub fn new(mut entries: Vec<ProviderEntry>) -> Self {
        entries.sort_by_key(|e| e.priority);
        let providers = entries
            .into_iter()
            .map(|e| {
                let label = e.label.clone()
                    .unwrap_or_else(|| format!("{}/{}", e.provider_type, e.model));
                (label, e.build())
            })
            .collect();
        Self { providers }
    }

}

impl LlmProvider for ProviderChain {
    fn complete<'a>(
        &'a self,
        system: &'a str,
        messages: &'a [Value],
        tools: &'a [Value],
        max_tokens: u32,
    ) -> Pin<Box<dyn Future<Output = ProviderResult> + Send + 'a>> {
        Box::pin(async move {
            let mut last_err = String::from("no providers configured");
            for (label, provider) in &self.providers {
                match provider.complete(system, messages, tools, max_tokens).await {
                    Ok(resp) if !resp.content.is_empty() => {
                        tracing::debug!("ProviderChain: success via {label}");
                        return Ok(resp);
                    }
                    Ok(_) => {
                        tracing::warn!("ProviderChain: empty response from {label}, trying next");
                        last_err = format!("{label}: empty response");
                    }
                    Err(e) => {
                        tracing::warn!("ProviderChain: {label} failed: {e}, trying next");
                        last_err = format!("{label}: {e}");
                    }
                }
            }
            Err(format!("all providers failed; last error: {last_err}"))
        })
    }
}

/// Parse provider list from the `LLM_PROVIDERS` env var.
///
/// Format: comma-separated entries, each field separated by `|` (pipe).
///   `type|url|api_key|model[|label[|priority]]`
/// Empty fields are allowed (omit url/key to inherit from env).
/// Example: `openai-compat|http://localhost:11434/v1||llama3,anthropic|||claude-opus-4-7|main|1`
///
/// Pipe is used instead of colon to avoid conflicts with `://` in URLs.
pub fn providers_from_env() -> Vec<ProviderEntry> {
    let raw = match std::env::var("LLM_PROVIDERS") {
        Ok(v) if !v.is_empty() => v,
        _ => return vec![],
    };
    let mut entries: Vec<ProviderEntry> = raw
        .split(',')
        .enumerate()
        .filter_map(|(i, part)| {
            let fields: Vec<&str> = part.trim().splitn(6, '|').collect();
            if fields.len() < 4 { return None; }
            let url = if fields[1].is_empty() { None } else { Some(fields[1].to_string()) };
            let api_key = if fields[2].is_empty() { None } else { Some(fields[2].to_string()) };
            let model = fields[3].to_string();
            let label = fields.get(4).filter(|s| !s.is_empty()).map(|s| s.to_string());
            let priority = fields.get(5)
                .and_then(|s| s.parse().ok())
                .unwrap_or(i as u8);
            Some(ProviderEntry {
                provider_type: fields[0].to_string(),
                url, api_key, model, label, priority,
            })
        })
        .collect();
    entries.sort_by_key(|e| e.priority);
    entries
}

/// Build the provider to use for inference.
///
/// Priority:
/// 1. `LLM_PROVIDERS` env var (multi-provider chain with fallthrough)
/// 2. `HERMES_PROVIDER=openai` / `OPENAI_BASE_URL` / `HERMES_BACKEND_URL` → single OpenAiProvider
/// 3. Default → single AnthropicProvider
pub fn make_provider(api_key: String, model: String) -> Box<dyn LlmProvider> {
    let chain_entries = providers_from_env();
    if !chain_entries.is_empty() {
        tracing::info!(
            "hermes-rust: using provider chain: {:?}",
            chain_entries.iter().map(|e| &e.model).collect::<Vec<_>>()
        );
        return Box::new(ProviderChain::new(chain_entries));
    }

    let use_openai = std::env::var("HERMES_PROVIDER").as_deref() == Ok("openai")
        || std::env::var("OPENAI_BASE_URL").is_ok()
        || std::env::var("HERMES_BACKEND_URL").is_ok();
    if use_openai {
        let oai_key = std::env::var("OPENAI_API_KEY")
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .unwrap_or(api_key);
        Box::new(OpenAiProvider::new(oai_key, model))
    } else {
        Box::new(AnthropicProvider::new(api_key, model))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // ── Minimal mock LLM servers ─────────────────────────────────────────────

    /// Spin up an axum server on a random port, return its URL and the
    /// recorded request bodies so tests can inspect what was sent.
    async fn mock_server(
        handler: Router,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}", addr);
        let handle = tokio::spawn(async move {
            axum::serve(listener, handler).await.ok();
        });
        (url, handle)
    }

    fn anthropic_mock_router(recorded: Arc<Mutex<Vec<Value>>>) -> Router {
        Router::new().route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let recorded = recorded.clone();
                async move {
                    recorded.lock().await.push(body);
                    Json(json!({
                        "content": [{"type": "text", "text": "mock reply"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 10, "output_tokens": 5}
                    }))
                }
            }),
        )
    }

    fn openai_mock_router(recorded: Arc<Mutex<Vec<Value>>>) -> Router {
        Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let recorded = recorded.clone();
                async move {
                    recorded.lock().await.push(body);
                    Json(json!({
                        "choices": [{"message": {"content": "oai reply", "role": "assistant"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 8, "completion_tokens": 3}
                    }))
                }
            }),
        )
    }

    // ── AnthropicProvider tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn anthropic_provider_returns_text_on_success() {
        let recorded = Arc::new(Mutex::new(vec![]));
        let (url, _h) = mock_server(anthropic_mock_router(recorded.clone())).await;
        let p = AnthropicProvider::with_base_url("key".into(), "claude-test".into(), url);
        let resp = p.complete("sys", &[], &[], 1024).await.unwrap();
        assert_eq!(resp.stop_reason, "end_turn");
        assert_eq!(resp.content[0]["text"], "mock reply");
        assert_eq!(resp.input_tokens, 10);
        assert_eq!(resp.output_tokens, 5);
    }

    #[tokio::test]
    async fn anthropic_provider_sends_tools_when_nonempty() {
        let recorded = Arc::new(Mutex::new(vec![]));
        let (url, _h) = mock_server(anthropic_mock_router(recorded.clone())).await;
        let p = AnthropicProvider::with_base_url("key".into(), "m".into(), url);
        let tools = vec![json!({"name":"bash","description":"run bash","input_schema":{"type":"object","properties":{}}})];
        p.complete("sys", &[], &tools, 512).await.unwrap();
        let req = recorded.lock().await[0].clone();
        assert!(req.get("tools").is_some(), "tools must be included when non-empty");
    }

    #[tokio::test]
    async fn anthropic_provider_omits_tools_when_empty() {
        let recorded = Arc::new(Mutex::new(vec![]));
        let (url, _h) = mock_server(anthropic_mock_router(recorded.clone())).await;
        let p = AnthropicProvider::with_base_url("key".into(), "m".into(), url);
        p.complete("sys", &[], &[], 512).await.unwrap();
        let req = recorded.lock().await[0].clone();
        assert!(req.get("tools").is_none(), "tools must be omitted when empty");
    }

    #[tokio::test]
    async fn anthropic_provider_returns_error_on_4xx() {
        // Return 401 from a plain axum handler
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        let router = Router::new().route(
            "/v1/messages",
            post(|| async { (StatusCode::UNAUTHORIZED, "Unauthorized").into_response() }),
        );
        let (url, _h) = mock_server(router).await;
        let p = AnthropicProvider::with_base_url("bad-key".into(), "m".into(), url);
        let result = p.complete("sys", &[], &[], 512).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("API error 401"));
    }

    // ── OpenAiProvider tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn openai_provider_returns_text_on_success() {
        let recorded = Arc::new(Mutex::new(vec![]));
        let (url, _h) = mock_server(openai_mock_router(recorded.clone())).await;
        let p = OpenAiProvider::with_base_url("key".into(), "gpt-test".into(), url);
        let resp = p.complete("sys", &[], &[], 1024).await.unwrap();
        assert_eq!(resp.stop_reason, "end_turn");
        assert_eq!(resp.content[0]["text"], "oai reply");
        assert_eq!(resp.input_tokens, 8);
        assert_eq!(resp.output_tokens, 3);
    }

    #[tokio::test]
    async fn openai_provider_translates_tool_calls_to_tool_use_blocks() {
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "choices": [{
                        "message": {
                            "content": null,
                            "role": "assistant",
                            "tool_calls": [{
                                "id": "call-1",
                                "type": "function",
                                "function": {"name": "bash", "arguments": "{\"command\":\"echo hi\"}"}
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 2}
                }))
            }),
        );
        let (url, _h) = mock_server(router).await;
        let p = OpenAiProvider::with_base_url("key".into(), "m".into(), url);
        let resp = p.complete("sys", &[], &[], 512).await.unwrap();
        assert_eq!(resp.stop_reason, "tool_use");
        assert_eq!(resp.content.len(), 1);
        assert_eq!(resp.content[0]["type"], "tool_use");
        assert_eq!(resp.content[0]["name"], "bash");
        assert_eq!(resp.content[0]["input"]["command"], "echo hi");
    }

    #[tokio::test]
    async fn openai_provider_normalizes_length_stop_to_max_tokens() {
        let router = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "choices": [{"message": {"content": "partial", "role": "assistant"}, "finish_reason": "length"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                }))
            }),
        );
        let (url, _h) = mock_server(router).await;
        let p = OpenAiProvider::with_base_url("key".into(), "m".into(), url);
        let resp = p.complete("sys", &[], &[], 512).await.unwrap();
        assert_eq!(resp.stop_reason, "max_tokens");
    }

    #[tokio::test]
    async fn openai_provider_sends_system_message_as_first_element() {
        let recorded = Arc::new(Mutex::new(vec![]));
        let (url, _h) = mock_server(openai_mock_router(recorded.clone())).await;
        let p = OpenAiProvider::with_base_url("key".into(), "m".into(), url);
        p.complete("my system prompt", &[], &[], 512).await.unwrap();
        let req = recorded.lock().await[0].clone();
        let messages = req["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "my system prompt");
    }

    // ── ProviderChain tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn provider_chain_returns_first_success() {
        let recorded = Arc::new(Mutex::new(vec![]));
        let (url, _h) = mock_server(openai_mock_router(recorded.clone())).await;
        let chain = ProviderChain {
            providers: vec![(
                "test".to_string(),
                Box::new(OpenAiProvider::with_base_url("key".into(), "m".into(), url))
                    as Box<dyn LlmProvider>,
            )],
        };
        let resp = chain.complete("sys", &[], &[], 512).await.unwrap();
        assert_eq!(resp.content[0]["text"], "oai reply");
    }

    #[tokio::test]
    async fn provider_chain_falls_through_on_error() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        // First provider always 500s
        let failing_router = Router::new()
            .route("/v1/chat/completions", post(|| async {
                (StatusCode::INTERNAL_SERVER_ERROR, "oops").into_response()
            }));
        let (fail_url, _h1) = mock_server(failing_router).await;

        // Second provider succeeds
        let recorded = Arc::new(Mutex::new(vec![]));
        let (ok_url, _h2) = mock_server(openai_mock_router(recorded.clone())).await;

        let chain = ProviderChain {
            providers: vec![
                (
                    "failing".to_string(),
                    Box::new(OpenAiProvider::with_base_url("key".into(), "m".into(), fail_url))
                        as Box<dyn LlmProvider>,
                ),
                (
                    "working".to_string(),
                    Box::new(OpenAiProvider::with_base_url("key".into(), "m".into(), ok_url))
                        as Box<dyn LlmProvider>,
                ),
            ],
        };
        let resp = chain.complete("sys", &[], &[], 512).await.unwrap();
        assert_eq!(resp.content[0]["text"], "oai reply");
        // Second provider was reached
        assert_eq!(recorded.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn provider_chain_returns_error_when_all_fail() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        let failing_router = Router::new()
            .route("/v1/chat/completions", post(|| async {
                (StatusCode::INTERNAL_SERVER_ERROR, "nope").into_response()
            }));
        let (url, _h) = mock_server(failing_router).await;

        let chain = ProviderChain {
            providers: vec![(
                "bad".to_string(),
                Box::new(OpenAiProvider::with_base_url("key".into(), "m".into(), url))
                    as Box<dyn LlmProvider>,
            )],
        };
        let result = chain.complete("sys", &[], &[], 512).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("all providers failed"));
    }

    // ── providers_from_env tests ─────────────────────────────────────────────
    // These tests exercise the parse_providers_str helper logic inline since
    // providers_from_env() reads from the real env var (global state, unsafe
    // to set in parallel tests). The format is pipe-delimited to avoid conflicts
    // with `://` in URLs.

    fn parse_providers_str(raw: &str) -> Vec<ProviderEntry> {
        let mut entries: Vec<ProviderEntry> = raw
            .split(',')
            .enumerate()
            .filter_map(|(i, part)| {
                let fields: Vec<&str> = part.trim().splitn(6, '|').collect();
                if fields.len() < 4 { return None; }
                let url = if fields[1].is_empty() { None } else { Some(fields[1].to_string()) };
                let api_key = if fields[2].is_empty() { None } else { Some(fields[2].to_string()) };
                let model = fields[3].to_string();
                let label = fields.get(4).filter(|s| !s.is_empty()).map(|s| s.to_string());
                let priority = fields.get(5).and_then(|s| s.parse().ok()).unwrap_or(i as u8);
                Some(ProviderEntry {
                    provider_type: fields[0].to_string(),
                    url, api_key, model, label, priority,
                })
            })
            .collect();
        entries.sort_by_key(|e| e.priority);
        entries
    }

    #[test]
    fn providers_from_env_parses_minimal_entry() {
        // Pipe-delimited so URLs with `://` aren't split incorrectly
        let entries = parse_providers_str("openai-compat|http://localhost:11434/v1|mykey|llama3");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].provider_type, "openai-compat");
        assert_eq!(entries[0].url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(entries[0].api_key.as_deref(), Some("mykey"));
        assert_eq!(entries[0].model, "llama3");
        assert!(entries[0].label.is_none());
        assert_eq!(entries[0].priority, 0);
    }

    #[test]
    fn providers_from_env_parses_empty_url_and_key() {
        let entries = parse_providers_str("anthropic|||claude-opus-4-7|anthropic-main|1");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].url.is_none(), "empty url field should produce None");
        assert!(entries[0].api_key.is_none(), "empty key field should produce None");
        assert_eq!(entries[0].label.as_deref(), Some("anthropic-main"));
        assert_eq!(entries[0].priority, 1);
    }

    #[test]
    fn providers_from_env_skips_entries_with_fewer_than_4_fields() {
        // Only 3 pipe-separated fields — should be skipped
        let result = parse_providers_str("openai|http://x.com|only-three");
        assert!(result.is_empty(), "entry with < 4 fields must be skipped");
    }

    #[test]
    fn providers_from_env_sorted_by_explicit_priority() {
        // Three entries with explicit priorities out of natural order
        let raw = "openai|||gpt-4o||10,anthropic|||claude-opus-4-7||5,openai-compat|http://x/v1||llama3||1";
        let entries = parse_providers_str(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].model, "llama3");       // priority 1
        assert_eq!(entries[1].model, "claude-opus-4-7"); // priority 5
        assert_eq!(entries[2].model, "gpt-4o");       // priority 10
    }

    #[test]
    fn providers_from_env_url_with_scheme_and_port_parsed_correctly() {
        // Verify that http://host:port/path is kept intact (the whole motivation
        // for switching from colon to pipe as the field delimiter)
        let entries = parse_providers_str("openai-compat|http://vllm.internal:8000/v1||mistral-7b");
        assert_eq!(entries[0].url.as_deref(), Some("http://vllm.internal:8000/v1"));
        assert_eq!(entries[0].model, "mistral-7b");
    }
}
