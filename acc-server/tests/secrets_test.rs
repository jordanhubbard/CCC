mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

// ── Auth guards ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_secrets_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/secrets").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_get_secret_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/secrets/MY_KEY").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_set_secret_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("POST").uri("/api/secrets/MY_KEY")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"value":"x"}).to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_delete_secret_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("DELETE").uri("/api/secrets/MY_KEY").body(Body::empty()).unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_secrets_empty() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(helpers::call(&ts.app, helpers::get("/api/secrets")).await).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["count"], 0);
    assert!(body["keys"].as_array().unwrap().is_empty());
}

// ── Set / Get ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_set_and_get_secret() {
    let ts = helpers::TestServer::new().await;
    let set_resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/secrets/API_TOKEN", &json!({"value": "hunter2"})),
    ).await;
    assert_eq!(set_resp.status(), StatusCode::OK);
    let set_body = helpers::body_json(set_resp).await;
    assert_eq!(set_body["ok"], true);
    assert_eq!(set_body["key"], "API_TOKEN");
    assert_eq!(set_body["value"], "hunter2");

    let get_body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/secrets/API_TOKEN")).await,
    ).await;
    assert_eq!(get_body["ok"], true);
    assert_eq!(get_body["value"], "hunter2");
}

#[tokio::test]
async fn test_set_secret_requires_value_field() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/secrets/BAD_KEY", &json!({})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_get_missing_secret_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/secrets/NO_SUCH_KEY")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_set_lists_key() {
    let ts = helpers::TestServer::new().await;
    helpers::call(
        &ts.app,
        helpers::post_json("/api/secrets/LIST_TARGET", &json!({"value": "42"})),
    ).await;
    let body = helpers::body_json(helpers::call(&ts.app, helpers::get("/api/secrets")).await).await;
    let keys = body["keys"].as_array().unwrap();
    assert!(keys.iter().any(|k| k == "LIST_TARGET"));
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_secret() {
    let ts = helpers::TestServer::new().await;
    helpers::call(
        &ts.app,
        helpers::post_json("/api/secrets/DEL_ME", &json!({"value": "gone"})),
    ).await;

    let del_body = helpers::body_json(
        helpers::call(&ts.app, helpers::delete("/api/secrets/DEL_ME")).await,
    ).await;
    assert_eq!(del_body["ok"], true);
    assert_eq!(del_body["deleted"], true);

    let get_resp = helpers::call(&ts.app, helpers::get("/api/secrets/DEL_ME")).await;
    assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_missing_secret_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/secrets/GHOST")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
