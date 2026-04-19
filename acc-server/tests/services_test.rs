//! Services and presence routes.
//! services_status probes real network endpoints — all will be offline in tests.
//! presence reads state.agents (AppState-backed, isolated per TestServer).
mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;

// ── /api/presence ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_presence_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/presence").body(Body::empty()).unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_presence_returns_object() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/presence").body(Body::empty()).unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, req).await).await;
    // No agents registered in fresh TestServer — empty map
    assert!(body.is_object(), "presence must return a JSON object, got: {body}");
}

// ── /api/services/status ──────────────────────────────────────────────────────

/// This test probes real network endpoints (all offline in CI).
/// It verifies the response shape, not service availability.
#[tokio::test]
async fn test_services_status_returns_array() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/services/status").body(Body::empty()).unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, req).await).await;
    // Returns an array of service probe results (each has id, name, online)
    let services = body.as_array().expect("services/status must return a JSON array");
    for svc in services {
        assert!(svc["id"].is_string());
        assert!(svc["name"].is_string());
        assert!(svc["online"].is_boolean());
    }
}
