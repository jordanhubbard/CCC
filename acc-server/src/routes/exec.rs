use crate::AppState;
/// /api/exec — Remote exec dispatch via AgentBus
///
/// POST /api/exec      — sign + broadcast exec via AgentBus, log to exec.jsonl
/// GET  /api/exec/:id  — retrieve exec record + results
/// POST /api/exec/:id/result — agent posts result back
///
/// Targeting: `targets` may contain agent names OR capability labels (e.g. "hermes",
/// "gpu"). Capability labels are resolved to agent names server-side by inspecting the
/// agents registry, then the bus message is broadcast with `to:"all"` so each agent
/// self-filters via body.targets.
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/exec", post(post_exec))
        .route("/api/exec/:id", get(get_exec))
        .route("/api/exec/:id/result", post(post_exec_result))
}

fn exec_log_path() -> String {
    std::env::var("EXEC_LOG_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        format!("{}/.local/state/acc/exec.jsonl", home)
    })
}

fn sign_payload(payload: &Value, secret: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(payload.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn server_name() -> String {
    std::env::var("AGENT_NAME")
        .or_else(|_| std::env::var("ACC_SERVER_NAME"))
        .unwrap_or_else(|_| "acc-server".to_string())
}

// ── POST /api/exec ────────────────────────────────────────────────────────

async fn post_exec(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }

    // Accept command+params (new) or code+mode (deprecated shell)
    let (exec_kind, payload_fields) = if let Some(cmd) =
        body.get("command").and_then(|v| v.as_str())
    {
        let params = body.get("params").cloned().unwrap_or_default();
        ("command", json!({"command": cmd, "params": params}))
    } else if let Some(code) = body.get("code").and_then(|v| v.as_str()) {
        if code.is_empty() {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error":"code must not be empty"})),
            )
                .into_response();
        }
        let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("shell");
        ("shell", json!({"code": code, "mode": mode}))
    } else {
        return (axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error":"request must include 'command' (registry) or 'code' (deprecated shell)"}))).into_response();
    };

    let agentbus_token = std::env::var("AGENTBUS_TOKEN")
        .or_else(|_| std::env::var("SQUIRRELBUS_TOKEN"))
        .unwrap_or_default();
    if agentbus_token.is_empty() {
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"AGENTBUS_TOKEN not configured"})),
        )
            .into_response();
    }

    let exec_id = format!("exec-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let timeout_ms = body
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(30_000);

    // Collect raw targets (agent names or capability labels)
    let raw_targets: Vec<String> = if let Some(arr) = body.get("targets").and_then(|v| v.as_array())
    {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    } else if let Some(t) = body.get("target").and_then(|v| v.as_str()) {
        vec![t.to_string()]
    } else {
        vec!["all".to_string()]
    };

    // Resolve capability labels → agent names
    let targets = resolve_targets(&state, raw_targets).await;

    // Build signed payload
    let mut payload = json!({
        "execId":     exec_id,
        "id":         exec_id,
        "targets":    targets,
        "timeout_ms": timeout_ms,
        "replyTo":    body.get("replyTo").cloned(),
        "ts":         now.clone(),
    });
    // Merge exec-kind fields (command+params or code+mode)
    if let (Some(p_obj), Some(e_obj)) = (payload.as_object_mut(), payload_fields.as_object()) {
        for (k, v) in e_obj {
            p_obj.insert(k.clone(), v.clone());
        }
    }

    let sig = sign_payload(&payload, &agentbus_token);
    let mut envelope = payload.clone();
    envelope
        .as_object_mut()
        .unwrap()
        .insert("sig".to_string(), json!(sig));

    // Fan-out: use `to:"all"` so every agent receives and self-filters via body.targets.
    // For a single named target we can be specific, but "all" is always correct.
    let bus_to = if targets.len() == 1 && targets[0] != "all" {
        targets[0].clone()
    } else {
        "all".to_string()
    };

    let bus_url = std::env::var("AGENTBUS_URL")
        .or_else(|_| std::env::var("SQUIRRELBUS_URL"))
        .unwrap_or_else(|_| {
            let port = std::env::var("ACC_PORT").unwrap_or_else(|_| "8789".to_string());
            format!("http://localhost:{port}")
        });
    let bus_token = std::env::var("ACC_AGENT_TOKEN").unwrap_or_else(|_| agentbus_token.clone());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let bus_sent = client
        .post(format!("{bus_url}/api/bus/send"))
        .bearer_auth(&bus_token)
        .json(&json!({
            "from":    server_name(),
            "to":      bus_to,
            "type":    "acc.exec",
            "subject": format!("acc.exec:{exec_id}"),
            "body":    envelope,
        }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    let log_record = json!({
        "execId":     exec_id.clone(),
        "id":         exec_id.clone(),
        "ts":         now,
        "kind":       exec_kind,
        "targets":    targets,
        "timeout_ms": timeout_ms,
        "replyTo":    body.get("replyTo").cloned(),
        "results":    [],
        "busSent":    bus_sent,
        "requestedBy": "admin",
    });
    append_exec_log(&log_record).await;

    (axum::http::StatusCode::OK,
        Json(json!({"ok": true, "id": exec_id, "execId": exec_id, "busSent": bus_sent, "targets": targets})))
        .into_response()
}

// ── GET /api/exec/:id ─────────────────────────────────────────────────────

async fn get_exec(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    match read_exec_record(&id).await {
        Some(record) => (axum::http::StatusCode::OK, Json(record)).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({"error":"not found"})),
        )
            .into_response(),
    }
}

// ── POST /api/exec/:id/result ─────────────────────────────────────────────

async fn post_exec_result(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let path = exec_log_path();
    let now = chrono::Utc::now().to_rfc3339();

    let mut records: Vec<Value> = match tokio::fs::read_to_string(&path).await {
        Ok(content) => content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        Err(_) => vec![],
    };

    let mut result_entry = body.clone();
    result_entry
        .as_object_mut()
        .unwrap()
        .insert("ts".to_string(), json!(now));

    match records
        .iter()
        .position(|r| r.get("execId").and_then(|v| v.as_str()) == Some(&id))
    {
        Some(i) => {
            records[i]
                .as_object_mut()
                .unwrap()
                .entry("results")
                .or_insert(json!([]))
                .as_array_mut()
                .unwrap()
                .push(result_entry);
        }
        None => {
            records.push(json!({
                "execId":  id.clone(),
                "ts":      now,
                "results": [result_entry],
                "stub":    true,
            }));
        }
    }

    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let content = records
        .iter()
        .filter_map(|r| serde_json::to_string(r).ok())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let _ = tokio::fs::write(&path, content).await;

    (
        axum::http::StatusCode::OK,
        Json(json!({"ok": true, "execId": id})),
    )
        .into_response()
}

// ── Capability resolution ─────────────────────────────────────────────────

/// Resolve raw target strings (agent names or capability labels) to concrete
/// agent names. "all" passes through unchanged. Unknown strings with no matching
/// capability are kept as-is (produces a warning at dispatch time).
async fn resolve_targets(state: &Arc<AppState>, raw: Vec<String>) -> Vec<String> {
    if raw.iter().any(|t| t == "all") {
        return vec!["all".to_string()];
    }

    let agents = state.agents.read().await;
    let agent_map = match agents.as_object() {
        Some(m) => m,
        None => return raw,
    };

    let mut resolved: Vec<String> = Vec::new();
    let mut unresolved_warnings: Vec<String> = Vec::new();

    for target in &raw {
        if agent_map.contains_key(target.as_str()) {
            // Exact agent name match
            resolved.push(target.clone());
        } else {
            // Treat as capability label — expand to all agents that have it
            let matched: Vec<String> = agent_map
                .iter()
                .filter(|(_, info)| agent_has_capability(info, target))
                .map(|(name, _)| name.clone())
                .collect();

            if matched.is_empty() {
                tracing::warn!(
                    target = %target,
                    "exec target '{}' is not a known agent name or capability — no agents matched",
                    target
                );
                unresolved_warnings.push(target.clone());
            } else {
                tracing::info!(
                    cap = %target,
                    agents = ?matched,
                    "resolved capability '{}' to {} agent(s)",
                    target, matched.len()
                );
                resolved.extend(matched);
            }
        }
    }

    resolved.sort();
    resolved.dedup();

    if resolved.is_empty() {
        // Nothing resolved: return originals so the caller gets a clear busSent:false
        raw
    } else {
        resolved
    }
}

fn agent_has_capability(agent_info: &Value, cap: &str) -> bool {
    match agent_info.get("capabilities") {
        Some(Value::Array(arr)) => arr.iter().any(|v| v.as_str() == Some(cap)),
        Some(Value::Object(map)) => map.get(cap).and_then(|v| v.as_bool()).unwrap_or(false),
        _ => false,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

async fn append_exec_log(record: &Value) {
    let path = exec_log_path();
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(mut line) = serde_json::to_string(record) {
        line.push('\n');
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            let _ = f.write_all(line.as_bytes()).await;
        }
    }
}

async fn read_exec_record(id: &str) -> Option<Value> {
    let path = exec_log_path();
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|r| r.get("execId").and_then(|v| v.as_str()) == Some(id))
}
