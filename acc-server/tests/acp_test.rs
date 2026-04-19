//! ACP session registry — process-global OnceLock store.
//! Tests use unique agent+session IDs to avoid cross-test interference.
mod helpers;

use axum::http::StatusCode;
use serde_json::json;

fn acp_session(id: &str, agent: &str) -> serde_json::Value {
    json!({
        "id":          id,
        "agent":       agent,
        "kind":        "claude-code",
        "started_at":  "2026-01-01T00:00:00Z",
        "last_active": "2026-01-01T00:00:00Z",
        "status":      "active",
    })
}

// ── List (no auth) ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_all_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder().method("GET").uri("/api/acp/sessions").body(Body::empty()).unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(body["sessions"].is_array());
    assert!(body["total"].is_number());
}

#[tokio::test]
async fn test_list_agent_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder().method("GET").uri("/api/acp/sessions/unknown-agent").body(Body::empty()).unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(body["sessions"].as_array().unwrap().is_empty());
}

// ── Register ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_register_requires_auth() {
    let ts = helpers::TestServer::new().await;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder()
        .method("POST").uri("/api/acp/sessions/test-agent-r")
        .header("Content-Type", "application/json")
        .body(Body::from(acp_session("s-r", "test-agent-r").to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_register_and_list_agent_sessions() {
    let ts = helpers::TestServer::new().await;
    let agent = "acp-reg-agent";
    let session_id = "sess-reg-001";

    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            &format!("/api/acp/sessions/{agent}"),
            &acp_session(session_id, agent),
        ),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(helpers::body_json(resp).await["ok"], true);

    // The list-agent endpoint doesn't require auth
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder()
        .method("GET").uri(&format!("/api/acp/sessions/{agent}")).body(Body::empty()).unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, req).await).await;
    let sessions = body["sessions"].as_array().unwrap();
    assert!(sessions.iter().any(|s| s["id"] == session_id));
}

// ── Update ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_update_session() {
    let ts = helpers::TestServer::new().await;
    let agent = "acp-upd-agent";
    let session_id = "sess-upd-001";

    helpers::call(
        &ts.app,
        helpers::post_json(&format!("/api/acp/sessions/{agent}"), &acp_session(session_id, agent)),
    ).await;

    let resp = helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/acp/sessions/{agent}/{session_id}"),
            &json!({"status": "idle", "label": "refactoring"}),
        ),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(helpers::body_json(resp).await["ok"], true);
}

#[tokio::test]
async fn test_update_nonexistent_session_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::put_json("/api/acp/sessions/ghost/no-sess", &json!({"status": "idle"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Remove ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_remove_session() {
    let ts = helpers::TestServer::new().await;
    let agent = "acp-del-agent";
    let session_id = "sess-del-001";

    helpers::call(
        &ts.app,
        helpers::post_json(&format!("/api/acp/sessions/{agent}"), &acp_session(session_id, agent)),
    ).await;

    let resp = helpers::call(
        &ts.app,
        helpers::delete(&format!("/api/acp/sessions/{agent}/{session_id}")),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_remove_nonexistent_session_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::delete("/api/acp/sessions/ghost/no-sess"),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
