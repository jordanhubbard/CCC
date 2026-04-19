mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;

#[tokio::test]
async fn test_supervisor_status_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/supervisor/status").body(Body::empty()).unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_supervisor_status_shape_when_disabled() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET").uri("/api/supervisor/status").body(Body::empty()).unwrap();
    let body = helpers::body_json(helpers::call(&ts.app, req).await).await;
    // TestServer sets supervisor: None — enabled=false, empty process list
    assert_eq!(body["processes"].as_array().unwrap().len(), 0);
    assert_eq!(body["enabled"], false);
}
