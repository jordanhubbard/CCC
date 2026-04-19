//! Issues routes: no auth guards, no direct create endpoint (requires `gh` CLI).
//! Tests cover list, 404 paths, and patch/delete on non-existent issues.
mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;

fn no_auth_get(path: &str) -> Request<Body> {
    Request::builder().method("GET").uri(path).body(Body::empty()).unwrap()
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_issues_ok() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/issues")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body["issues"].is_array());
    assert!(body["count"].is_number());
}

#[tokio::test]
async fn test_list_issues_state_filter() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/issues?state=closed")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    // All returned issues should be closed (or empty)
    for issue in body["issues"].as_array().unwrap() {
        assert_eq!(issue["state"], "closed");
    }
}

// ── Get by number ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_issue_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/issues/999999")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Patch ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_patch_issue_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::put_json("/api/issues/999999", &serde_json::json!({"state": "closed"})),
    ).await;
    // patch_issue uses PATCH (put_json uses PUT) — actual method mismatch will 405
    // The route is PATCH, so this is a method-not-allowed scenario
    assert!(
        resp.status() == StatusCode::NOT_FOUND
            || resp.status() == StatusCode::METHOD_NOT_ALLOWED,
        "unexpected: {}",
        resp.status()
    );
}

#[tokio::test]
async fn test_patch_issue_with_correct_method() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("PATCH").uri("/api/issues/999999")
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::json!({"state": "closed"}).to_string()))
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_issue_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/issues/999999")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
