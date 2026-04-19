mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

#[tokio::test]
async fn test_setup_status_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/setup/status").body(Body::empty()).unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_setup_status_shape() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/setup/status").body(Body::empty()).unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, req).await).await;
    assert!(body["version"].is_string());
    assert!(body["first_run"].is_boolean());
    assert!(body["has_accfs"].is_boolean());
}

#[tokio::test]
async fn test_setup_config_get_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/setup/config").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_setup_config_get_shape() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/setup/config")).await,
    ).await;
    assert!(body["agent_name"].is_string());
    assert!(body["tokenhub_url"].is_string());
    assert!(body["ccc_port"].is_number());
}

#[tokio::test]
async fn test_setup_config_put_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("PUT").uri("/api/setup/config")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"agent_name": "test"}).to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}
