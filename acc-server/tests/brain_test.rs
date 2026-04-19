mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

// ── Status ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_brain_status_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/brain/status")
        .body(Body::empty())
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_brain_status_shape() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/brain/status")
        .body(Body::empty())
        .unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, req).await).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["queueDepth"], 0);
    assert_eq!(body["completedCount"], 0);
    assert!(body["backend"].is_string());
}

// ── Brain request ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_brain_request_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/brain/request")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"messages": []}).to_string()))
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_brain_request_requires_messages_field() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/brain/request", &json!({})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_brain_request_accepted_and_queued() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/brain/request", &json!({
            "messages": [{"role": "user", "content": "hello"}]
        })),
    ).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], "queued");
    assert!(body["requestId"].as_str().unwrap().starts_with("brain-"));
}

#[tokio::test]
async fn test_brain_request_increments_queue_depth() {
    let ts = helpers::TestServer::new().await;
    helpers::call(
        &ts.app,
        helpers::post_json("/api/brain/request", &json!({
            "messages": [{"role": "user", "content": "task 1"}]
        })),
    ).await;
    helpers::call(
        &ts.app,
        helpers::post_json("/api/brain/request", &json!({
            "messages": [{"role": "user", "content": "task 2"}]
        })),
    ).await;

    let status_req = Request::builder()
        .method("GET")
        .uri("/api/brain/status")
        .body(Body::empty())
        .unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, status_req).await).await;
    assert_eq!(body["queueDepth"], 2);
}

#[tokio::test]
async fn test_brain_request_with_priority() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/brain/request", &json!({
            "messages": [{"role": "user", "content": "urgent"}],
            "priority": "high",
            "maxTokens": 512,
        })),
    ).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body = helpers::body_json(resp).await;
    assert!(body["requestId"].is_string());
}

#[tokio::test]
async fn test_brain_request_ids_are_unique() {
    let ts = helpers::TestServer::new().await;
    let msg = json!({"messages": [{"role": "user", "content": "x"}]});
    let r1 = helpers::body_json(
        helpers::call(&ts.app, helpers::post_json("/api/brain/request", &msg)).await,
    ).await;
    let r2 = helpers::body_json(
        helpers::call(&ts.app, helpers::post_json("/api/brain/request", &msg)).await,
    ).await;
    assert_ne!(r1["requestId"], r2["requestId"]);
}
