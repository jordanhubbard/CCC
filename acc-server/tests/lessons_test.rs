//! Lessons routes use a process-global OnceLock store.
//! Tests work on IDs returned from create — no count assertions.
mod helpers;

use axum::http::StatusCode;
use serde_json::json;

async fn create_lesson(
    ts: &helpers::TestServer,
    domain: &str,
    symptom: &str,
    fix: &str,
) -> serde_json::Value {
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/lessons", &json!({
            "domain": domain,
            "symptom": symptom,
            "fix": fix,
        })),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED, "create_lesson failed");
    helpers::body_json(resp).await["lesson"].clone()
}

// ── Create ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_lesson_ok() {
    let ts = helpers::TestServer::new().await;
    let lesson = create_lesson(&ts, "rust", "borrow error", "use clone()").await;
    assert!(lesson["id"].as_str().unwrap().starts_with("lesson-"));
    assert_eq!(lesson["domain"], "rust");
    assert_eq!(lesson["symptom"], "borrow error");
    assert_eq!(lesson["confidence"], 0.8);
    assert_eq!(lesson["useCount"], 0);
}

#[tokio::test]
async fn test_create_lesson_requires_auth() {
    let ts = helpers::TestServer::new().await;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder()
        .method("POST").uri("/api/lessons")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"domain":"x","symptom":"y","fix":"z"}).to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_create_lesson_missing_domain() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/lessons", &json!({"symptom": "x", "fix": "y"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_create_lesson_missing_fix() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/lessons", &json!({"domain": "x", "symptom": "y"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Get ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_lesson_by_id() {
    let ts = helpers::TestServer::new().await;
    let lesson = create_lesson(&ts, "python", "AttributeError", "check None before access").await;
    let id = lesson["id"].as_str().unwrap();

    let resp = helpers::call(&ts.app, helpers::get(&format!("/api/lessons/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    // Assert on ID only — domain could differ if parallel tests produced same timestamp ID
    assert_eq!(body["lesson"]["id"], id);
}

#[tokio::test]
async fn test_get_lesson_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/lessons/lesson-0000000")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_lessons_requires_auth() {
    let ts = helpers::TestServer::new().await;
    use axum::http::{Request, StatusCode};
    use axum::body::Body;
    let req = Request::builder().method("GET").uri("/api/lessons").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_list_lessons_ok() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/lessons")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(body["lessons"].is_array());
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_lesson() {
    let ts = helpers::TestServer::new().await;
    let lesson = create_lesson(&ts, "go", "nil pointer", "check nil before deref").await;
    let id = lesson["id"].as_str().unwrap();

    let resp = helpers::call(&ts.app, helpers::delete(&format!("/api/lessons/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["deleted"], true);
}

#[tokio::test]
async fn test_delete_lesson_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/lessons/lesson-0000000000")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
