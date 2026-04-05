/// /api/exec — Remote exec dispatch via ClawBus (Rust port of services.mjs exec routes)
///
/// POST /api/exec      — sign + broadcast exec via ClawBus, log to exec.jsonl
/// GET  /api/exec/:id  — retrieve exec record + results
/// POST /api/exec/:id/result — agent posts result back

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
use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/exec", post(post_exec))
        .route("/api/exec/:id", get(get_exec))
        .route("/api/exec/:id/result", post(post_exec_result))
}

fn exec_log_path() -> String {
    std::env::var("EXEC_LOG_PATH").unwrap_or_else(|_| "./data/exec.jsonl".to_string())
}

fn sign_payload(payload: &Value, secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(payload.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

// ── POST /api/exec ────────────────────────────────────────────────────────

async fn post_exec(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let code = match body.get("code").and_then(|v| v.as_str()) {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error":"code required"}))).into_response(),
    };

    let clawbus_token = std::env::var("CLAWBUS_TOKEN")
        .or_else(|_| std::env::var("SQUIRRELBUS_TOKEN"))  // backwards compat
        .unwrap_or_default();
    if clawbus_token.is_empty() {
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"CLAWBUS_TOKEN not configured"}))).into_response();
    }

    let exec_id = format!("exec-{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("shell").to_string();
    let timeout_ms = body.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(30000);

    // Support targets (array) or target (string) for backward compat
    let targets: Vec<String> = if let Some(arr) = body.get("targets").and_then(|v| v.as_array()) {
        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
    } else if let Some(t) = body.get("target").and_then(|v| v.as_str()) {
        vec![t.to_string()]
    } else {
        vec!["all".to_string()]
    };

    let payload = json!({
        "execId":     exec_id,
        "id":         exec_id,
        "code":       code,
        "targets":    targets,
        "mode":       mode,
        "timeout_ms": timeout_ms,
        "replyTo":    body.get("replyTo").cloned(),
        "ts":         now.clone(),
    });
    let sig = sign_payload(&payload, &clawbus_token);
    let envelope = {
        let mut e = payload.clone();
        e.as_object_mut().unwrap().insert("sig".to_string(), json!(sig));
        e
    };

    // Broadcast via ClawBus
    let bus_url = std::env::var("CLAWBUS_URL")
        .or_else(|_| std::env::var("SQUIRRELBUS_URL"))  // backwards compat
        .unwrap_or_else(|_| format!("http://localhost:{}", std::env::var("RCC_PORT").unwrap_or_else(|_| "8789".to_string())));
    let bus_token = std::env::var("CCC_AGENT_TOKEN").unwrap_or(clawbus_token.clone());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let broadcast_to = targets.first().map(|s| s.as_str()).unwrap_or("all");
    let bus_sent = client
        .post(format!("{}/api/bus/send", bus_url))
        .bearer_auth(&bus_token)
        .json(&json!({
            "from":    "rocky",
            "to":      broadcast_to,
            "type":    "rcc.exec",
            "subject": format!("rcc.exec:{}", exec_id),
            "body":    envelope.to_string(),
        }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    // Log to exec.jsonl
    let log_record = json!({
        "execId":      exec_id.clone(),
        "id":          exec_id.clone(),
        "ts":          now,
        "code":        code,
        "targets":     targets,
        "mode":        mode,
        "timeout_ms":  timeout_ms,
        "replyTo":     body.get("replyTo").cloned(),
        "results":     [],
        "busSent":     bus_sent,
        "requestedBy": "admin",
    });
    append_exec_log(&log_record).await;

    (axum::http::StatusCode::OK, Json(json!({"ok": true, "id": exec_id, "execId": exec_id, "busSent": bus_sent}))).into_response()
}

// ── GET /api/exec/:id ─────────────────────────────────────────────────────

async fn get_exec(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    match read_exec_record(&id).await {
        Some(record) => (axum::http::StatusCode::OK, Json(record)).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"Exec record not found"}))).into_response(),
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
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let path = exec_log_path();
    let now = chrono::Utc::now().to_rfc3339();

    // Read all records
    let mut records: Vec<Value> = match tokio::fs::read_to_string(&path).await {
        Ok(content) => content.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        Err(_) => vec![],
    };

    let result_entry = {
        let mut r = body.clone();
        r.as_object_mut().unwrap().insert("ts".to_string(), json!(now));
        r
    };

    let idx = records.iter().position(|r| r.get("execId").and_then(|v| v.as_str()) == Some(&id));
    match idx {
        Some(i) => {
            records[i].as_object_mut().unwrap()
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

    // Rewrite file
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let content: String = records.iter()
        .filter_map(|r| serde_json::to_string(r).ok())
        .collect::<Vec<_>>()
        .join("\n") + "\n";
    let _ = tokio::fs::write(&path, content).await;

    (axum::http::StatusCode::OK, Json(json!({"ok": true, "execId": id}))).into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────

async fn append_exec_log(record: &Value) {
    let path = exec_log_path();
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Ok(mut line) = serde_json::to_string(record) {
        line.push('\n');
        if let Ok(mut f) = tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await {
            let _ = f.write_all(line.as_bytes()).await;
        }
    }
}

async fn read_exec_record(id: &str) -> Option<Value> {
    let path = exec_log_path();
    let content = tokio::fs::read_to_string(&path).await.ok()?;
    content.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|r| r.get("execId").and_then(|v| v.as_str()) == Some(id))
}
