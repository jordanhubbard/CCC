mod helpers;

use axum::http::StatusCode;
use serde_json::json;

// Use /api/secrets which is auth-gated for all methods.

#[tokio::test]
async fn no_token_returns_401() {
    let srv = helpers::TestServer::new().await;

    // Request without Authorization header should be rejected by auth-gated endpoints
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/secrets")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = helpers::call(&srv.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_token_returns_401() {
    let srv = helpers::TestServer::new().await;

    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/secrets")
        .header("Authorization", "Bearer wrong-token")
        .body(axum::body::Body::empty())
        .unwrap();

    let resp = helpers::call(&srv.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn valid_token_allows_access() {
    let srv = helpers::TestServer::new().await;
    // Use a write-gated endpoint with valid token
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/queue",
            &json!({
                "title": "auth-test-item",
                "description": "testing auth grants access",
                "_skip_dedup": true,
            }),
        ),
    )
    .await;
    // POST /api/queue returns 201 CREATED
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn health_is_public() {
    // /api/health should work without auth
    let srv = helpers::TestServer::new().await;
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/api/health")
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = helpers::call(&srv.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
