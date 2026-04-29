mod helpers;

use axum::http::StatusCode;
use serde_json::json;

async fn create_project(ts: &helpers::TestServer, name: &str) -> serde_json::Value {
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/projects", &json!({"name": name})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "create_project({name}) failed"
    );
    helpers::body_json(resp).await["project"].clone()
}

async fn create_project_with(
    ts: &helpers::TestServer,
    body: serde_json::Value,
) -> serde_json::Value {
    let resp = helpers::call(&ts.app, helpers::post_json("/api/projects", &body)).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    helpers::body_json(resp).await["project"].clone()
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_projects_empty() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/projects")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 0);
    assert!(body["projects"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_list_projects_returns_all() {
    let ts = helpers::TestServer::new().await;
    create_project(&ts, "Alpha").await;
    create_project(&ts, "Beta").await;
    let resp = helpers::call(&ts.app, helpers::get("/api/projects")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["projects"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_list_filter_by_status() {
    let ts = helpers::TestServer::new().await;
    let p = create_project(&ts, "Active One").await;
    let id = p["id"].as_str().unwrap();
    create_project(&ts, "Active Two").await;
    // Archive one
    helpers::call(&ts.app, helpers::delete(&format!("/api/projects/{id}"))).await;

    let resp = helpers::call(&ts.app, helpers::get("/api/projects?status=active")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["projects"][0]["name"], "Active Two");

    let resp = helpers::call(&ts.app, helpers::get("/api/projects?status=archived")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["projects"][0]["name"], "Active One");
}

#[tokio::test]
async fn test_list_filter_by_tag() {
    let ts = helpers::TestServer::new().await;
    create_project_with(
        &ts,
        json!({"name": "Rust project", "tags": ["rust", "backend"]}),
    )
    .await;
    create_project_with(
        &ts,
        json!({"name": "Frontend app", "tags": ["js", "frontend"]}),
    )
    .await;

    let resp = helpers::call(&ts.app, helpers::get("/api/projects?tag=rust")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["projects"][0]["name"], "Rust project");
}

#[tokio::test]
async fn test_list_search_by_name() {
    let ts = helpers::TestServer::new().await;
    create_project(&ts, "Fleet Dashboard").await;
    create_project(&ts, "Agent Worker").await;

    let resp = helpers::call(&ts.app, helpers::get("/api/projects?q=fleet")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["projects"][0]["name"], "Fleet Dashboard");
}

#[tokio::test]
async fn test_list_search_case_insensitive() {
    let ts = helpers::TestServer::new().await;
    create_project(&ts, "My Project").await;

    let resp = helpers::call(&ts.app, helpers::get("/api/projects?q=MY")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 1);
}

#[tokio::test]
async fn test_list_pagination_limit_offset() {
    let ts = helpers::TestServer::new().await;
    for i in 0..5 {
        create_project(&ts, &format!("Project {i}")).await;
    }

    let resp = helpers::call(&ts.app, helpers::get("/api/projects?limit=2&offset=0")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 5);
    assert_eq!(body["projects"].as_array().unwrap().len(), 2);

    let resp = helpers::call(&ts.app, helpers::get("/api/projects?limit=2&offset=4")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["total"], 5);
    assert_eq!(body["projects"].as_array().unwrap().len(), 1);
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
        .method("POST")
        .uri("/api/projects")
        .header("Content-Type", "application/json")
        .body(Body::from(json!({"name": "Test"}).to_string()))
        .unwrap();
    assert_eq!(
        helpers::call(&ts.app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn test_create_project_name_required() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::post_json("/api/projects", &json!({}))).await;
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
        helpers::patch_json(
            &format!("/api/projects/{id}"),
            &json!({"description": "Updated!"}),
        ),
    )
    .await;
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
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Delete (soft archive) ──────────────────────────────────────────────────────

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

    // Still shows up in unfiltered list
    let list_resp = helpers::call(&ts.app, helpers::get("/api/projects")).await;
    let list = helpers::body_json(list_resp).await;
    assert_eq!(list["total"], 1);
}

#[tokio::test]
async fn test_delete_project_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/projects/no-such-id")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Delete (hard) ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_hard_delete_removes_project() {
    let ts = helpers::TestServer::new().await;
    let project = create_project(&ts, "Purge Me").await;
    let id = project["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::delete(&format!("/api/projects/{id}?hard=true")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body.get("deleted").is_some());

    // Must be gone from list
    let list_resp = helpers::call(&ts.app, helpers::get("/api/projects")).await;
    let list = helpers::body_json(list_resp).await;
    assert_eq!(list["total"], 0);
}

#[tokio::test]
async fn test_hard_delete_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::delete("/api/projects/no-such-id?hard=true"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
