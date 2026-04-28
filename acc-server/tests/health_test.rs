mod helpers;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn health_returns_ok() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/health")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    // /api/health returns {"ok": true, "service": "acc-server"}
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["service"], json!("acc-server"));
}

#[tokio::test]
async fn status_returns_uptime() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/status")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(
        body.get("uptime_secs").is_some(),
        "expected uptime_secs field"
    );
}
