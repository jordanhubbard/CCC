//! Metrics routes: OnceLock store, scoped by owner/repo path segment.
//! Tests use unique repo names to avoid cross-test data bleeding.
mod helpers;

use axum::http::StatusCode;
use serde_json::json;

async fn post_metric(
    ts: &helpers::TestServer,
    owner: &str,
    repo: &str,
    metric: &str,
    value: f64,
) -> axum::http::Response<axum::body::Body> {
    helpers::call(
        &ts.app,
        helpers::post_json(
            &format!("/api/projects/{owner}/{repo}/metrics"),
            &json!({"metric": metric, "value": value}),
        ),
    ).await
}

// ── Auth guards ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_post_metric_requires_auth() {
    let ts = helpers::TestServer::new().await;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder()
        .method("POST").uri("/api/projects/org/repo/metrics")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"metric":"cov","value":80.0}).to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_get_metrics_requires_auth() {
    let ts = helpers::TestServer::new().await;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder()
        .method("GET").uri("/api/projects/org/repo/metrics")
        .body(Body::empty())
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

// ── Input validation ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_post_metric_requires_metric_field() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/projects/org/repo/metrics", &json!({"value": 90.0})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_post_metric_requires_value_field() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/projects/org/repo/metrics", &json!({"metric": "coverage_pct"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Post and get ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_post_metric_accepted() {
    let ts = helpers::TestServer::new().await;
    let resp = post_metric(&ts, "acme", "metrics-test-1", "coverage_pct", 87.5).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["entry"]["metric"], "coverage_pct");
    assert_eq!(body["entry"]["value"], 87.5);
    assert_eq!(body["entry"]["repo"], "acme/metrics-test-1");
}

#[tokio::test]
async fn test_get_metrics_for_repo() {
    let ts = helpers::TestServer::new().await;
    post_metric(&ts, "acme", "metrics-test-2", "test_count", 42.0).await;

    let resp = helpers::call(
        &ts.app,
        helpers::get("/api/projects/acme/metrics-test-2/metrics"),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    // Response shape: {repo, count, entries, sparklines} — no "ok" field
    assert_eq!(body["repo"], "acme/metrics-test-2");
    let entries = body["entries"].as_array().unwrap();
    assert!(entries.iter().any(|e| e["metric"] == "test_count" && e["value"] == 42.0));
}

#[tokio::test]
async fn test_get_metrics_empty_for_unknown_repo() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::get("/api/projects/nobody/totally-unique-empty-repo-xyz/metrics"),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(body["entries"].as_array().unwrap().is_empty());
}
