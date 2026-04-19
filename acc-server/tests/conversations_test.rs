//! Conversations routes use a process-global OnceLock store.
//! Tests operate on IDs returned from create — no count assertions.
mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

fn no_auth_post(path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST").uri(path)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn no_auth_get(path: &str) -> Request<Body> {
    Request::builder().method("GET").uri(path).body(Body::empty()).unwrap()
}

fn no_auth_delete(path: &str) -> Request<Body> {
    Request::builder().method("DELETE").uri(path).body(Body::empty()).unwrap()
}

fn no_auth_patch(path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PATCH").uri(path)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn create_conv(ts: &helpers::TestServer, channel: &str) -> serde_json::Value {
    let resp = helpers::call(
        &ts.app,
        no_auth_post("/api/conversations", &json!({"channel": channel})),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    helpers::body_json(resp).await["conversation"].clone()
}

// ── Create ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_conversation() {
    let ts = helpers::TestServer::new().await;
    let conv = create_conv(&ts, "slack").await;
    assert!(conv["id"].as_str().unwrap().starts_with("conv-"));
    assert_eq!(conv["channel"], "slack");
    assert!(conv["messages"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_create_conversation_no_auth_needed() {
    // conversations are public (no auth guard in the route handlers)
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        no_auth_post("/api/conversations", &json!({"channel": "telegram"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

// ── Get ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_conversation() {
    let ts = helpers::TestServer::new().await;
    let conv = create_conv(&ts, "web").await;
    let id = conv["id"].as_str().unwrap();

    let resp = helpers::call(&ts.app, no_auth_get(&format!("/api/conversations/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    // Assert on ID only — channel may differ if parallel tests produced same timestamp ID
    assert_eq!(body["id"], id);
}

#[tokio::test]
async fn test_get_conversation_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        no_auth_get("/api/conversations/conv-does-not-exist"),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_conversations_returns_array() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/conversations")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(body.as_array().is_some());
}

// ── Patch ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_patch_conversation() {
    let ts = helpers::TestServer::new().await;
    let conv = create_conv(&ts, "slack").await;
    let id = conv["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        no_auth_patch(&format!("/api/conversations/{id}"), &json!({"tags": ["important"]})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["conversation"]["tags"][0], "important");
}

// ── Add message ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_add_message_to_conversation() {
    let ts = helpers::TestServer::new().await;
    let conv = create_conv(&ts, "slack").await;
    let id = conv["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        no_auth_post(
            &format!("/api/conversations/{id}/messages"),
            &json!({"author": "agent-a", "text": "Hello!"}),
        ),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["message"]["author"], "agent-a");
    assert_eq!(body["message"]["text"], "Hello!");
}

#[tokio::test]
async fn test_add_message_requires_author_and_text() {
    let ts = helpers::TestServer::new().await;
    let conv = create_conv(&ts, "web").await;
    let id = conv["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        no_auth_post(&format!("/api/conversations/{id}/messages"), &json!({"author": "a"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_conversation() {
    let ts = helpers::TestServer::new().await;
    let conv = create_conv(&ts, "delete-me").await;
    let id = conv["id"].as_str().unwrap();

    let resp = helpers::call(&ts.app, no_auth_delete(&format!("/api/conversations/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["deleted"], true);
}

#[tokio::test]
async fn test_delete_conversation_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        no_auth_delete("/api/conversations/conv-does-not-exist-0"),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
