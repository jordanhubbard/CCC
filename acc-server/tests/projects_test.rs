mod helpers;

use axum::http::StatusCode;
use serde_json::json;

async fn create_project(ts: &helpers::TestServer, name: &str) -> serde_json::Value {
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/projects", &json!({"name": name})),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED, "create_project({name}) failed");
    helpers::body_json(resp).await["project"].clone()
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_projects_empty() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/projects")).await;
    // list_projects has no auth gate — returns array directly
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(body.as_array().is_some());
}

// ── Create ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_project_ok() {
    let ts = helpers::TestServer::new().await;
    let project = create_project(&ts, "My Project").await;
    assert_eq!(project["name"], "My Project");
    assert_eq!(project["slug"], "my-project");
    assert_eq!(project["status"], "active");
    assert!(project["id"].as_str().unwrap().starts_with("proj-"));
    assert_eq!(project["clone_status"], "none");
}

#[tokio::test]
async fn test_create_project_requires_auth() {
    let ts = helpers::TestServer::new().await;
    use axum::body::Body;
    use axum::http::Request;
    let req = Request::builder()
        .method("POST").uri("/api/projects")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"name": "Test"}).to_string()))
        .unwrap();
    assert_eq!(helpers::call(&ts.app, req).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_create_project_name_required() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/projects", &json!({})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_create_project_slug_computed() {
    let ts = helpers::TestServer::new().await;
    let project = create_project(&ts, "Hello World 123!").await;
    assert_eq!(project["slug"], "hello-world-123");
}

// ── Get ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_project_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/projects/nobody/nothing")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Update ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_update_project() {
    let ts = helpers::TestServer::new().await;
    let project = create_project(&ts, "Update Me").await;
    let id = project["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::patch_json(&format!("/api/projects/{id}"), &json!({"description": "Updated!"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["project"]["description"], "Updated!");
}

#[tokio::test]
async fn test_update_project_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::patch_json("/api/projects/no-such-id", &json!({"description": "nope"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Delete (archive) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_project_archives_it() {
    let ts = helpers::TestServer::new().await;
    let project = create_project(&ts, "Archive Me").await;
    let id = project["id"].as_str().unwrap();

    let resp = helpers::call(&ts.app, helpers::delete(&format!("/api/projects/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["project"]["status"], "archived");
}

#[tokio::test]
async fn test_delete_project_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/projects/no-such-id")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
