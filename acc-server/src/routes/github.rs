//! POST /api/github/webhook — real-time GitHub issue event handler (F3)
//!
//! Validates X-Hub-Signature-256 HMAC, then routes:
//!   issues.opened / issues.labeled  → notify bus; create fleet task if agent-ready
//!   issues.closed                   → publish tasks:github_issue_closed on bus
//!   issues.edited                   → publish tasks:github_issue_edited on bus
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/github/webhook", post(handle_webhook))
}

fn verify_signature(secret: &str, body: &[u8], sig_header: &str) -> bool {
    use std::fmt::Write;
    // sig_header is "sha256=<hex>"
    let Some(hex_sig) = sig_header.strip_prefix("sha256=") else {
        return false;
    };
    // Compute HMAC-SHA256
    let key = hmac_sha256_key(secret.as_bytes(), body);
    let computed = bytes_to_hex(&key);
    // Constant-time compare
    constant_time_eq(computed.as_bytes(), hex_sig.as_bytes())
}

fn hmac_sha256_key(key: &[u8], msg: &[u8]) -> [u8; 32] {
    // Pure-Rust HMAC-SHA256 using the sha2 crate.
    use sha2::Sha256;
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length ok");
    mac.update(msg);
    mac.finalize().into_bytes().into()
}

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().fold(String::new(), |mut s, byte| {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{:02x}", byte));
        s
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn handle_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let secret = std::env::var("GITHUB_WEBHOOK_SECRET").unwrap_or_default();
    if !secret.is_empty() {
        let sig = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !verify_signature(&secret, &body, sig) {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid signature"}))).into_response();
        }
    }

    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid JSON"}))).into_response(),
    };

    if event_type != "issues" {
        return (StatusCode::OK, Json(json!({"ok": true, "skipped": true}))).into_response();
    }

    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let issue = payload.get("issue").cloned().unwrap_or(json!({}));
    let repo = payload
        .pointer("/repository/full_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let number = issue.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
    let title = issue.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let dispatch_label = std::env::var("GITHUB_DISPATCH_LABEL")
        .unwrap_or_else(|_| "agent-ready".to_string());

    let bus_msg = match action {
        "opened" => {
            tracing::info!("github webhook: issue opened {}#{} \"{}\"", repo, number, title);
            json!({
                "type": "github:issue_opened",
                "repo": repo,
                "number": number,
                "title": title,
                "url": issue.get("html_url").cloned().unwrap_or(json!("")),
            })
        }
        "labeled" => {
            let label_name = payload
                .pointer("/label/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            tracing::info!("github webhook: {}#{} labeled {:?}", repo, number, label_name);
            if label_name == dispatch_label {
                // Trigger github-sync.py for this specific issue via bus nudge
                let _ = state.bus_tx.send(json!({
                    "type": "github:dispatch_label_added",
                    "repo": repo,
                    "number": number,
                    "title": title,
                    "label": label_name,
                }).to_string());
                tracing::info!("github webhook: dispatch nudge sent for {}#{}", repo, number);
            }
            json!({
                "type": "github:issue_labeled",
                "repo": repo,
                "number": number,
                "label": label_name,
            })
        }
        "closed" => {
            tracing::info!("github webhook: issue closed {}#{}", repo, number);
            json!({
                "type": "github:issue_closed",
                "repo": repo,
                "number": number,
                "title": title,
            })
        }
        "edited" => {
            json!({
                "type": "github:issue_edited",
                "repo": repo,
                "number": number,
                "title": title,
            })
        }
        _ => {
            return (StatusCode::OK, Json(json!({"ok": true, "action": action, "skipped": true}))).into_response();
        }
    };

    let _ = state.bus_tx.send(bus_msg.to_string());
    (StatusCode::OK, Json(json!({"ok": true, "action": action}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature_valid() {
        let secret = "mysecret";
        let body = b"hello world";
        // Compute expected sig
        let key = hmac_sha256_key(secret.as_bytes(), body);
        let hex = bytes_to_hex(&key);
        let sig_header = format!("sha256={}", hex);
        assert!(verify_signature(secret, body, &sig_header));
    }

    #[test]
    fn test_verify_signature_invalid() {
        assert!(!verify_signature("secret", b"body", "sha256=deadbeef"));
    }

    #[test]
    fn test_verify_signature_wrong_prefix() {
        assert!(!verify_signature("secret", b"body", "md5=abc123"));
    }

    #[test]
    fn test_verify_signature_empty_secret_skipped() {
        // When secret is empty, caller skips verification — tested at handler level
        assert!(!verify_signature("", b"body", "sha256=anything"));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"ab", b"abc"));
    }
}
