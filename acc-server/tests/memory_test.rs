//! Integration tests for /api/memory/* and /api/vector/* routes.
//!
//! These endpoints require Qdrant (vector DB) and an embedding API — neither
//! are available in the test environment. Tests therefore cover:
//!   - Auth guards: every protected endpoint returns 401 without a token
//!   - Input validation: handlers with early-exit checks return 400 without
//!     ever reaching the external service
//!   - vector_health: no auth required; returns 200 or 500 (never 4xx)
mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

// ── memory/ingest ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_ingest_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/memory/ingest")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"text": "hello"}).to_string()))
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_memory_ingest_empty_text_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/memory/ingest", &json!({"text": ""})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = helpers::body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("text"));
}

// ── memory/ingest/bulk ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_ingest_bulk_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/memory/ingest/bulk")
        .header("Content-Type", "application/json")
        .body(Body::from("[]"))
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── memory/recall ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_recall_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/memory/recall?q=test")
        .body(Body::empty())
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_memory_recall_empty_q_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/memory/recall?q=")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── memory/recent ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_recent_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/memory/recent")
        .body(Body::empty())
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── memory/context ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_context_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/memory/context")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"query": "hello"}).to_string()))
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_memory_context_empty_query_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/memory/context", &json!({"query": ""})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = helpers::body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("query"));
}

// ── vector/health ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_vector_health_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/vector/health")
        .body(Body::empty())
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    // Returns 200 when Qdrant is available, 500 when not — never 4xx
    assert!(
        resp.status() == StatusCode::OK || resp.status() == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected status: {}",
        resp.status()
    );
}

#[tokio::test]
async fn test_vector_health_response_has_ok_field() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/vector/health")
        .body(Body::empty())
        .unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, req).await).await;
    assert!(body["ok"].is_boolean(), "response must have 'ok' boolean, got: {body}");
}

// ── vector/search ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_vector_search_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/vector/search?q=test")
        .body(Body::empty())
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_vector_search_empty_q_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/vector/search?q=")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── vector/upsert ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_vector_upsert_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/vector/upsert")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({"collection": "c", "id": "1", "text": "hello"}).to_string()
        ))
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_vector_upsert_missing_fields_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/vector/upsert", &json!({"collection": "c"})),
    ).await;
    // Axum's JSON extractor returns 422 when required struct fields (id, text) are absent
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_vector_upsert_empty_text_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/vector/upsert", &json!({
            "collection": "test",
            "id": "doc-1",
            "text": ""
        })),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
