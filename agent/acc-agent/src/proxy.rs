//! NVIDIA header-stripping HTTP proxy.
//!
//! Replaces nvidia-proxy.py. Listens on 127.0.0.1:9099 and forwards all
//! requests to NVIDIA_API_BASE, stripping the `anthropic-beta` header that
//! NVIDIA's LiteLLM endpoint rejects.
//!
//! Also sanitizes malformed message histories: if an assistant message
//! contains tool_use blocks without corresponding tool_result blocks in the
//! next user message, synthetic tool_result blocks are injected. This prevents
//! HTTP 400 errors from Bedrock ("tool_use ids were found without tool_result
//! blocks immediately after").

use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Request, State},
    response::Response,
    routing::any,
    Router,
};
use bytes::Bytes;
use http_body_util::BodyExt;
use reqwest::Client;
use serde_json::{json, Value};

const STRIP_HEADERS: &[&str] = &[
    "anthropic-beta",
    "host",
    "content-length",
    "transfer-encoding",
];

#[derive(Clone)]
struct ProxyState {
    client: Client,
    upstream: String,
}

pub async fn run(args: &[String]) {
    crate::config::load_env_file(&crate::config::acc_dir().join(".env"));

    let mut port: u16 = 9099;
    let mut upstream = std::env::var("NVIDIA_API_BASE")
        .unwrap_or_else(|_| "https://inference-api.nvidia.com".into());

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    port = p.parse().unwrap_or(9099);
                }
            }
            "--target" => {
                i += 1;
                if let Some(t) = args.get(i) {
                    upstream = t.clone();
                }
            }
            _ => {}
        }
        i += 1;
    }

    // ACC_PROXY_PORT env var overrides CLI default (set in .env for port conflicts)
    if let Some(env_port) = std::env::var("ACC_PROXY_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        port = env_port;
    }

    let upstream = upstream.trim_end_matches('/').to_string();

    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .expect("failed to build proxy client");

    let state = ProxyState {
        client,
        upstream: upstream.clone(),
    };

    let app = Router::new()
        .route("/", any(proxy_handler))
        .route("/*path", any(proxy_handler))
        .with_state(state);

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    eprintln!("[proxy] listening on {addr} → {upstream}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind proxy port");

    axum::serve(listener, app)
        .await
        .expect("proxy server error");
}

async fn proxy_handler(State(state): State<ProxyState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();

    let body_bytes: Bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            return error_response(400, &format!("body read error: {e}"));
        }
    };

    // Sanitize messages on /v1/messages requests to prevent Bedrock 400 errors
    // caused by orphaned tool_use blocks (no tool_result in the following message).
    let path = parts.uri.path();
    let body_bytes = if path.ends_with("/v1/messages") || path == "/v1/messages" {
        sanitize_body(body_bytes)
    } else {
        body_bytes
    };

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    // Avoid double /v1 when NVIDIA_API_BASE already includes the version prefix
    let effective_path = if state.upstream.ends_with("/v1") && path_and_query.starts_with("/v1/") {
        &path_and_query[3..]
    } else {
        path_and_query
    };
    let url = format!("{}{}", state.upstream, effective_path);

    let method: reqwest::Method = match parts.method.as_str().parse() {
        Ok(m) => m,
        Err(_) => reqwest::Method::GET,
    };

    let mut fwd_headers = reqwest::header::HeaderMap::new();
    for (name, value) in parts.headers.iter() {
        if STRIP_HEADERS.contains(&name.as_str()) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            fwd_headers.insert(n, v);
        }
    }

    let upstream_resp = match state
        .client
        .request(method, &url)
        .headers(fwd_headers)
        .body(body_bytes)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return error_response(502, &format!("upstream error: {e}")),
    };

    let status = upstream_resp.status().as_u16();
    let resp_headers = upstream_resp.headers().clone();
    let stream = upstream_resp.bytes_stream();

    let mut resp = Response::builder().status(status);
    for (name, value) in resp_headers.iter() {
        const HOP: &[&str] = &[
            "connection",
            "keep-alive",
            "transfer-encoding",
            "te",
            "trailer",
            "upgrade",
            "proxy-authorization",
            "proxy-authenticate",
        ];
        if HOP.contains(&name.as_str()) {
            continue;
        }
        resp = resp.header(name.as_str(), value.as_bytes());
    }

    resp.body(Body::from_stream(stream))
        .unwrap_or_else(|_| error_response(500, "response build error"))
}

/// Parse the request body and apply two fixes before forwarding:
///
/// 1. Convert any OpenAI-format `tool_calls` in assistant messages to Anthropic
///    `tool_use` content blocks.  This is the root cause of the
///    "Unable to convert openai tool calls" HTTP 500s from NVIDIA LiteLLM when
///    a conversation history contains Anthropic `tooluse_*` IDs wrapped in the
///    OpenAI `{type:"function", function:{name, arguments}}` envelope.
///
/// 2. Inject synthetic `tool_result` user messages for any assistant `tool_use`
///    blocks that are not followed by a matching `tool_result` (Bedrock 400 fix).
fn sanitize_body(body: Bytes) -> Bytes {
    let mut payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return body,
    };

    let messages = match payload.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(m) => m,
        None => return body,
    };

    // Step 1: normalize OpenAI-format tool_calls → Anthropic tool_use
    let normalized = normalize_openai_tool_calls(messages);
    if normalized > 0 {
        eprintln!("[proxy] normalized {normalized} OpenAI-format tool_calls → Anthropic tool_use");
    }

    // Step 2: inject synthetic tool_results for any orphaned tool_use blocks
    let injected = inject_missing_tool_results(messages);
    if injected > 0 {
        eprintln!(
            "[proxy] injected {injected} synthetic tool_result(s) for orphaned tool_use blocks"
        );
    }

    if normalized == 0 && injected == 0 {
        return body;
    }

    match serde_json::to_vec(&payload) {
        Ok(b) => Bytes::from(b),
        Err(_) => body,
    }
}

/// Convert assistant messages that carry OpenAI-format `tool_calls` arrays into
/// Anthropic-native `content` arrays with `tool_use` blocks.
///
/// OpenAI shape:
///   { role: "assistant", content: null, tool_calls: [{ id, type: "function",
///     function: { name, arguments: "<json string>" } }] }
///
/// Anthropic shape:
///   { role: "assistant", content: [{ type: "tool_use", id, name, input: {...} }] }
fn normalize_openai_tool_calls(messages: &mut Vec<Value>) -> usize {
    let mut converted = 0;
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let tool_calls = match msg.get("tool_calls").and_then(|tc| tc.as_array()) {
            Some(tc) if !tc.is_empty() => tc.clone(),
            _ => continue,
        };

        let mut content_blocks: Vec<Value> = Vec::new();

        // Preserve any existing text content
        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                content_blocks.push(json!({"type": "text", "text": text}));
            }
        }

        for tc in &tool_calls {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let args = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("{}");
            let input: Value = serde_json::from_str(args).unwrap_or(json!({}));
            content_blocks.push(json!({
                "type":  "tool_use",
                "id":    id,
                "name":  name,
                "input": input,
            }));
        }

        if let Some(obj) = msg.as_object_mut() {
            obj.remove("tool_calls");
            obj.insert("content".into(), json!(content_blocks));
            converted += 1;
        }
    }
    converted
}

/// Walk the messages array and inject synthetic tool_result user messages for
/// any orphaned tool_use blocks. Returns the number of tool_use IDs fixed.
fn inject_missing_tool_results(messages: &mut Vec<Value>) -> usize {
    let mut fixed = 0;
    let mut i = 0;
    while i < messages.len() {
        let tool_use_ids = collect_tool_use_ids(&messages[i]);
        if tool_use_ids.is_empty() {
            i += 1;
            continue;
        }

        // Collect which IDs already have a tool_result in the next message
        let covered: std::collections::HashSet<String> = if i + 1 < messages.len() {
            collect_tool_result_ids(&messages[i + 1])
                .into_iter()
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        let missing: Vec<String> = tool_use_ids
            .iter()
            .filter(|id| !covered.contains(*id))
            .cloned()
            .collect();

        if missing.is_empty() {
            i += 1;
            continue;
        }

        // Build synthetic tool_result blocks for each missing ID
        let result_blocks: Vec<Value> = missing
            .iter()
            .map(|id| {
                json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": "[result unavailable — session was interrupted before this tool completed]",
                    "is_error": true
                })
            })
            .collect();

        let synthetic_msg = json!({
            "role": "user",
            "content": result_blocks
        });

        fixed += missing.len();
        messages.insert(i + 1, synthetic_msg);
        i += 2; // skip past both the assistant message and the newly inserted user message
    }
    fixed
}

/// Return the tool_use IDs from an assistant message's content blocks.
fn collect_tool_use_ids(msg: &Value) -> Vec<String> {
    if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return vec![];
    }
    let content = match msg.get("content").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return vec![],
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|block| {
            block
                .get("id")
                .and_then(|id| id.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// Return the tool_use IDs covered by tool_result blocks in a user message.
fn collect_tool_result_ids(msg: &Value) -> Vec<String> {
    if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
        return vec![];
    }
    let content = match msg.get("content").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return vec![],
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        .filter_map(|block| {
            block
                .get("tool_use_id")
                .and_then(|id| id.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

fn error_response(status: u16, msg: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Body::from(msg.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_normalize_openai_tool_calls_to_anthropic() {
        let mut msgs = vec![
            json!({"role": "user", "content": "run something"}),
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "tooluse_Zx9feJ3w3ME71La2q8dHhv",
                    "type": "function",
                    "function": {
                        "name": "terminal",
                        "arguments": "{\"command\": \"echo hello\"}"
                    }
                }]
            }),
        ];
        let n = normalize_openai_tool_calls(&mut msgs);
        assert_eq!(n, 1);
        // tool_calls removed
        assert!(msgs[1].get("tool_calls").is_none());
        // content now has a tool_use block
        let content = msgs[1]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["id"], "tooluse_Zx9feJ3w3ME71La2q8dHhv");
        assert_eq!(content[0]["name"], "terminal");
        assert_eq!(content[0]["input"]["command"], "echo hello");
    }

    #[test]
    fn test_normalize_preserves_text_content() {
        let mut msgs = vec![json!({
            "role": "assistant",
            "content": "I'll run that for you.",
            "tool_calls": [{
                "id": "tu_1",
                "type": "function",
                "function": {"name": "bash", "arguments": "{}"}
            }]
        })];
        normalize_openai_tool_calls(&mut msgs);
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "I'll run that for you.");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn test_normalize_skips_native_anthropic() {
        let mut msgs = vec![json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": "tu_1", "name": "bash", "input": {}}
        ]})];
        assert_eq!(normalize_openai_tool_calls(&mut msgs), 0);
    }

    #[test]
    fn test_strip_headers_list() {
        assert!(STRIP_HEADERS.contains(&"anthropic-beta"));
        assert!(STRIP_HEADERS.contains(&"host"));
        assert!(STRIP_HEADERS.contains(&"content-length"));
    }

    #[test]
    fn test_no_tool_use_unchanged() {
        let mut msgs = vec![
            json!({"role": "user", "content": "hello"}),
            json!({"role": "assistant", "content": "hi"}),
        ];
        assert_eq!(inject_missing_tool_results(&mut msgs), 0);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn test_paired_tool_use_unchanged() {
        let mut msgs = vec![
            json!({"role": "user", "content": "go"}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_1", "name": "bash", "input": {}}
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"}
            ]}),
        ];
        assert_eq!(inject_missing_tool_results(&mut msgs), 0);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn test_orphaned_tool_use_gets_synthetic_result() {
        let mut msgs = vec![
            json!({"role": "user", "content": "go"}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_orphan", "name": "bash", "input": {}}
            ]}),
            // No tool_result message follows — the next message is a new user turn
            json!({"role": "user", "content": "what happened?"}),
        ];
        let fixed = inject_missing_tool_results(&mut msgs);
        assert_eq!(fixed, 1);
        assert_eq!(msgs.len(), 4);
        // Injected message should be at index 2
        assert_eq!(msgs[2]["role"], "user");
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "tu_orphan");
        assert_eq!(content[0]["is_error"], true);
    }

    #[test]
    fn test_multiple_orphaned_tool_uses_single_synthetic() {
        let mut msgs = vec![
            json!({"role": "user", "content": "go"}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_a", "name": "bash", "input": {}},
                {"type": "tool_use", "id": "tu_b", "name": "read", "input": {}}
            ]}),
        ];
        let fixed = inject_missing_tool_results(&mut msgs);
        assert_eq!(fixed, 2);
        assert_eq!(msgs.len(), 3);
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn test_partially_covered_tool_use() {
        let mut msgs = vec![
            json!({"role": "user", "content": "go"}),
            json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_a", "name": "bash", "input": {}},
                {"type": "tool_use", "id": "tu_b", "name": "read", "input": {}}
            ]}),
            // Only tu_a is covered
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_a", "content": "ok"}
            ]}),
        ];
        let fixed = inject_missing_tool_results(&mut msgs);
        assert_eq!(fixed, 1);
        assert_eq!(msgs.len(), 4);
        let content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(content[0]["tool_use_id"], "tu_b");
    }
}
