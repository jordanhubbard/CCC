mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

async fn create_task(srv: &helpers::TestServer, project_id: &str, title: &str) -> serde_json::Value {
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/tasks", &json!({"project_id": project_id, "title": title})),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED, "create_task failed");
    helpers::body_json(resp).await["task"].clone()
}

async fn claim(srv: &helpers::TestServer, id: &str, agent: &str) -> axum::http::Response<Body> {
    helpers::call(
        &srv.app,
        helpers::put_json(&format!("/api/tasks/{id}/claim"), &json!({"agent": agent})),
    ).await
}

// ── Create ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_task_ok() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/tasks", &json!({"project_id": "proj-1", "title": "Do something"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["task"]["project_id"], "proj-1");
    assert_eq!(body["task"]["title"], "Do something");
    assert_eq!(body["task"]["status"], "open");
    assert_eq!(body["task"]["priority"], 2);
}

#[tokio::test]
async fn test_create_task_missing_project_id() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/tasks", &json!({"title": "Oops"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_create_task_missing_title() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/tasks", &json!({"project_id": "p1"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_tasks_empty() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/tasks")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["count"], 0);
    assert!(body["tasks"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_list_tasks_requires_auth() {
    let ts = helpers::TestServer::new().await;
    let req = Request::builder().method("GET").uri("/api/tasks").body(Body::empty()).unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_list_tasks_filtered_by_status() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Filter test").await;
    let id = task["id"].as_str().unwrap();
    claim(&ts, id, "agent-a").await;

    let resp = helpers::call(&ts.app, helpers::get("/api/tasks?status=claimed")).await;
    let body = helpers::body_json(resp).await;
    let tasks = body["tasks"].as_array().unwrap();
    assert!(tasks.iter().all(|t| t["status"] == "claimed"));
    assert!(tasks.iter().any(|t| t["id"] == id));
}

// ── Get ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_task_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/tasks/nonexistent-id")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_get_task_found() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "proj-1", "Findable task").await;
    let id = task["id"].as_str().unwrap();

    let resp = helpers::call(&ts.app, helpers::get(&format!("/api/tasks/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["id"], id);
    assert_eq!(body["title"], "Findable task");
    assert_eq!(body["status"], "open");
}

// ── Claim ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_claim_task() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Claimable").await;
    let id = task["id"].as_str().unwrap();

    let resp = claim(&ts, id, "agent-a").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["task"]["status"], "claimed");
    assert_eq!(body["task"]["claimed_by"], "agent-a");
    assert!(body["task"]["claimed_at"].is_string());
    assert!(body["task"]["claim_expires_at"].is_string());
}

#[tokio::test]
async fn test_claim_task_requires_agent_field() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Needs agent").await;
    let id = task["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{id}/claim"), &json!({})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_claim_nonexistent_task_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::put_json("/api/tasks/no-such-id/claim", &json!({"agent": "agent-a"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_double_claim_returns_409() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Race target").await;
    let id = task["id"].as_str().unwrap();

    let r1 = claim(&ts, id, "agent-a").await;
    assert_eq!(r1.status(), StatusCode::OK);

    let r2 = claim(&ts, id, "agent-b").await;
    assert_eq!(r2.status(), StatusCode::CONFLICT);
    let body = helpers::body_json(r2).await;
    assert_eq!(body["error"], "already_claimed");
}

#[tokio::test]
async fn test_same_agent_double_claim_returns_409() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Same agent double").await;
    let id = task["id"].as_str().unwrap();

    claim(&ts, id, "agent-a").await;
    let r2 = claim(&ts, id, "agent-a").await;
    assert_eq!(r2.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn test_agent_at_capacity_returns_429() {
    let ts = helpers::TestServer::new().await;
    let agent = "overloaded-agent";
    let mut ids = Vec::new();
    for i in 0..4 {
        let task = create_task(&ts, "p1", &format!("Task {i}")).await;
        ids.push(task["id"].as_str().unwrap().to_string());
    }
    for id in &ids[..3] {
        let r = claim(&ts, id, agent).await;
        assert_eq!(r.status(), StatusCode::OK, "claim {id} failed");
    }
    let r = claim(&ts, &ids[3], agent).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = helpers::body_json(r).await;
    assert_eq!(body["error"], "agent_at_capacity");
    assert_eq!(body["max"], 3);
}

// ── Unclaim ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_unclaim_returns_task_to_open_pool() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Unclaim me").await;
    let id = task["id"].as_str().unwrap();

    claim(&ts, id, "agent-a").await;

    let unclaim_resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{id}/unclaim"), &json!({"agent": "agent-a"})),
    ).await;
    assert_eq!(unclaim_resp.status(), StatusCode::OK);

    // Should now be claimable by another agent
    let r = claim(&ts, id, "agent-b").await;
    assert_eq!(r.status(), StatusCode::OK);
}

// ── Complete ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_complete_task() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Complete me").await;
    let id = task["id"].as_str().unwrap();

    claim(&ts, id, "agent-a").await;

    let resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{id}/complete"), &json!({"agent": "agent-a"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["task"]["status"], "completed");
    assert_eq!(body["task"]["completed_by"], "agent-a");
    assert!(body["task"]["completed_at"].is_string());
}

#[tokio::test]
async fn test_complete_unclaimed_task_is_allowed() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Skip claim step").await;
    let id = task["id"].as_str().unwrap();

    // complete_task WHERE status IN ('claimed','in_progress','open') — open is included
    let resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{id}/complete"), &json!({"agent": "agent-a"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(helpers::body_json(resp).await["task"]["status"], "completed");
}

// ── Cancel ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_cancel_task() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Cancel me").await;
    let id = task["id"].as_str().unwrap();

    let resp = helpers::call(&ts.app, helpers::delete(&format!("/api/tasks/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);

    let get_resp = helpers::call(&ts.app, helpers::get(&format!("/api/tasks/{id}"))).await;
    assert_eq!(helpers::body_json(get_resp).await["status"], "cancelled");
}

#[tokio::test]
async fn test_cancel_nonexistent_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/tasks/no-such-id")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Update ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_update_task_title() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Old title").await;
    let id = task["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{id}"), &json!({"title": "New title"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(helpers::body_json(resp).await["task"]["title"], "New title");
}
