//! Model deployment routes — all require auth.
//! In the test environment: node process fails silently (script not found),
//! all GPU agent ports are unreachable, log files are empty → status "queued".
mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

// ── POST /api/models/deploy ───────────────────────────────────────────────────

#[tokio::test]
async fn test_trigger_deploy_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST").uri("/api/models/deploy")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"model_id": "google/gemma-4-31B-it"}).to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_trigger_deploy_rejects_invalid_model_id() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/models/deploy", &json!({"model_id": "notavalidmodelid"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_trigger_deploy_rejects_empty_model_id() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/models/deploy", &json!({"model_id": ""})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_trigger_deploy_accepts_hf_slash_path() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/models/deploy", &json!({"model_id": "google/gemma-4-31B-it"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["status"], "queued");
    assert!(body["deploy_id"].as_str().unwrap().starts_with("deploy-"));
    assert_eq!(body["model_id"], "google/gemma-4-31B-it");
}

#[tokio::test]
async fn test_trigger_deploy_accepts_meta_llama_prefix() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/models/deploy", &json!({"model_id": "meta-llama-3.1-8B"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn test_trigger_deploy_dry_run_flag() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/models/deploy", &json!({"model_id": "mistral/7b", "dry_run": true})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["dry_run"], true);
}

// ── GET /api/models/deploy/:id ────────────────────────────────────────────────

#[tokio::test]
async fn test_get_deploy_status_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/models/deploy/deploy-12345")
        .body(Body::empty())
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_get_deploy_status_shape() {
    let ts = helpers::TestServer::new().await;
    // First create a deploy to get a valid ID.
    let deploy_body = helpers::body_json(
        helpers::call(
            &ts.app,
            helpers::post_json("/api/models/deploy", &json!({"model_id": "openai/gpt-status-test"})),
        ).await,
    ).await;
    let deploy_id = deploy_body["deploy_id"].as_str().unwrap();

    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get(&format!("/api/models/deploy/{}", deploy_id))).await,
    ).await;
    assert_eq!(body["deploy_id"], deploy_id);
    assert!(body["status"].is_string());
    assert!(body["log_lines"].is_number());
    assert!(body["log_tail"].is_array());
    assert!(body["log_path"].is_string());
}

#[tokio::test]
async fn test_get_deploy_status_queued_for_new_deploy() {
    let ts = helpers::TestServer::new().await;
    let deploy_body = helpers::body_json(
        helpers::call(
            &ts.app,
            helpers::post_json("/api/models/deploy", &json!({"model_id": "acme/new-model"})),
        ).await,
    ).await;
    let deploy_id = deploy_body["deploy_id"].as_str().unwrap();

    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get(&format!("/api/models/deploy/{}", deploy_id))).await,
    ).await;
    // Log file is empty or absent immediately after dispatch → status is "queued" or "running".
    let status = body["status"].as_str().unwrap();
    assert!(
        ["queued", "running", "failed"].contains(&status),
        "expected queued/running/failed, got: {status}"
    );
}

#[tokio::test]
async fn test_get_deploy_status_unknown_id_returns_queued() {
    // Unknown deploy_id → log file doesn't exist → empty content → status "queued".
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/models/deploy/deploy-0000000000000")).await,
    ).await;
    assert_eq!(body["status"], "queued");
    assert_eq!(body["log_lines"], 0);
}

// ── GET /api/models/current ───────────────────────────────────────────────────

#[tokio::test]
async fn test_current_models_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/models/current").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_current_models_shape() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/models/current")).await,
    ).await;
    let nodes = body["nodes"].as_array().expect("current_models must return {nodes:[...]}");
    assert!(!nodes.is_empty());
    for node in nodes {
        assert!(node["agent"].is_string(), "each node must have agent");
        assert!(node["port"].is_number(),  "each node must have port");
        assert!(node["status"].is_string(), "each node must have status");
        assert!(node["models"].is_array(), "each node must have models array");
    }
}

#[tokio::test]
async fn test_current_models_all_unreachable_in_tests() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/models/current")).await,
    ).await;
    for node in body["nodes"].as_array().unwrap() {
        assert_eq!(node["status"], "unreachable", "GPU agents are offline in test env");
        assert!(node["models"].as_array().unwrap().is_empty());
    }
}
