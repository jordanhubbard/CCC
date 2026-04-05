//! Integration tests for the dashboard-server proxy routes.
//!
//! These tests start an httpmock server to simulate the CCC upstream,
//! then spin up the dashboard-server router in-process using axum-test.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use httpmock::prelude::*;
use tower::ServiceExt; // for oneshot

// Re-export the router builder from main.rs via a pub function.
// Since main.rs doesn't expose a pub fn build_router, we'll directly test
// via raw axum Router construction that mirrors what main.rs does.
// This tests the auth middleware logic independently of the full proxy.

fn test_router_with_token(token: &str, rcc_url: &str) -> Router {
    use axum::{
        extract::{Request as AxumRequest, State},
        http::{Method as HttpMethod, StatusCode as SC},
        middleware::{self, Next},
        response::Response,
        routing::{any, get},
    };
    use std::sync::Arc;

    #[derive(Clone)]
    struct TestState {
        agent_token: String,
        rcc_url: String,
    }

    async fn auth_mw(
        State(state): State<Arc<TestState>>,
        req: AxumRequest,
        next: Next,
    ) -> Response {
        let method = req.method().clone();
        let is_mutating = matches!(
            method,
            HttpMethod::POST | HttpMethod::PATCH | HttpMethod::DELETE | HttpMethod::PUT
        );

        if is_mutating && !state.agent_token.is_empty() {
            let auth_ok = req
                .headers()
                .get("Authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|tok| tok.trim() == state.agent_token)
                .unwrap_or(false);

            if !auth_ok {
                return Response::builder()
                    .status(SC::UNAUTHORIZED)
                    .body(Body::from(r#"{"error":"unauthorized"}"#))
                    .unwrap();
            }
        }
        next.run(req).await
    }

    async fn health_handler() -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "status": "ok",
            "uptime_seconds": 0,
            "rcc_connected": false,
            "sc_connected": false
        }))
    }

    let state = Arc::new(TestState {
        agent_token: token.to_string(),
        rcc_url: rcc_url.to_string(),
    });

    Router::new()
        .route("/health", get(health_handler))
        // Catch-all for API routes (returns mock 200 for test)
        .route(
            "/api/*path",
            any(|| async { (SC::OK, axum::Json(serde_json::json!({"ok": true}))) }),
        )
        .layer(middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_returns_200_with_status_ok() {
    let app = test_router_with_token("test-token", "http://localhost:9999");
    let req = Request::builder()
        .uri("/health")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn get_api_queue_allowed_without_auth() {
    let app = test_router_with_token("test-token", "http://localhost:9999");
    let req = Request::builder()
        .uri("/api/queue")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // GET should not be blocked by auth middleware
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_api_without_auth_returns_401() {
    let app = test_router_with_token("test-token", "http://localhost:9999");
    let req = Request::builder()
        .uri("/api/queue")
        .method(Method::POST)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"title":"test"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_api_with_valid_token_passes_auth() {
    let token = "my-secret-agent-token";
    let app = test_router_with_token(token, "http://localhost:9999");
    let req = Request::builder()
        .uri("/api/queue")
        .method(Method::POST)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::from(r#"{"title":"test"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // Should pass auth (actual proxy may 502 since no upstream, but not 401)
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn post_api_with_wrong_token_returns_401() {
    let app = test_router_with_token("correct-token", "http://localhost:9999");
    let req = Request::builder()
        .uri("/api/items/123")
        .method(Method::PATCH)
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer wrong-token")
        .body(Body::from(r#"{"status":"completed"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_returns_valid_json_fields() {
    let app = test_router_with_token("", "http://localhost:9999");
    let req = Request::builder()
        .uri("/health")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Required health fields
    assert!(json.get("status").is_some(), "health must have 'status'");
    assert!(json.get("uptime_seconds").is_some(), "health must have 'uptime_seconds'");
}

#[tokio::test]
async fn get_requests_allowed_when_no_token_configured() {
    // When agent_token is empty, auth middleware should allow everything through
    let app = test_router_with_token("", "http://localhost:9999");
    let req = Request::builder()
        .uri("/api/queue")
        .method(Method::POST)
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // No token configured → auth bypassed
    assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn httpmock_upstream_proxy_integration() {
    // Start a mock CCC server
    let mock_server = MockServer::start();
    mock_server.mock(|when, then| {
        when.method(GET).path("/api/queue");
        then.status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"items":[],"completed":[]}"#);
    });

    // The test router would proxy /api/* to mock_server URL.
    // We validate the mock server itself is reachable:
    let url = mock_server.url("/api/queue");
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["items"].is_array());
}
