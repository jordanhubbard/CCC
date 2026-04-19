//! UI routes: bootstrap token issuer and grievances proxy.
//! Bootstrap GET checks state.secrets["bootstrap/token"] — empty in fresh TestServer → always 401.
//! Grievances proxy target is not running → 502.
mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

// ── GET /api/bootstrap ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_bootstrap_no_token_param_is_401() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/bootstrap").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_bootstrap_wrong_token_is_401() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/bootstrap?token=totally-wrong")
        .body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_bootstrap_empty_secrets_is_always_401() {
    // Fresh TestServer has no secrets set — the global bootstrap token is empty.
    // An empty token never matches so even ?token= is rejected.
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/bootstrap?token=")
        .body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_bootstrap_correct_token_returns_200() {
    // POST /api/bootstrap/token with name="token" stores the key as "bootstrap/token",
    // which is exactly what GET /api/bootstrap reads for the global check.
    let ts = helpers::TestServer::new().await;
    let created = helpers::body_json(
        helpers::call(
            &ts.app,
            helpers::post_json("/api/bootstrap/token", &json!({"name": "token"})),
        ).await,
    ).await;
    let token = created["token"].as_str().unwrap();

    let req = Request::builder()
        .method("GET").uri(format!("/api/bootstrap?token={}", token))
        .body(Body::empty()).unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body["ccc_url"].is_string());
    assert!(body["tokenhub_url"].is_string());
}

// ── POST /api/bootstrap/token ─────────────────────────────────────────────────

#[tokio::test]
async fn test_post_bootstrap_token_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST").uri("/api/bootstrap/token")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"name": "myagent"}).to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_post_bootstrap_token_requires_name() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/bootstrap/token", &json!({})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_post_bootstrap_token_rejects_empty_name() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/bootstrap/token", &json!({"name": ""})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_post_bootstrap_token_creates_token_with_bt_prefix() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/bootstrap/token", &json!({"name": "myagent"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["agent"], "myagent");
    let token = body["token"].as_str().unwrap();
    assert!(token.starts_with("bt-myagent-"), "token must start with bt-{{name}}-");
}

#[tokio::test]
async fn test_post_bootstrap_token_two_calls_produce_different_tokens() {
    let ts = helpers::TestServer::new().await;
    let t1 = helpers::body_json(
        helpers::call(&ts.app, helpers::post_json("/api/bootstrap/token", &json!({"name": "a"}))).await,
    ).await["token"].as_str().unwrap().to_string();
    let t2 = helpers::body_json(
        helpers::call(&ts.app, helpers::post_json("/api/bootstrap/token", &json!({"name": "b"}))).await,
    ).await["token"].as_str().unwrap().to_string();
    assert_ne!(t1, t2);
}

// ── Grievances proxy ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_grievances_proxy_returns_502() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/grievances").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn test_api_grievances_proxy_returns_502() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/grievances").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::BAD_GATEWAY);
}
