mod helpers;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

async fn create_task(
    srv: &helpers::TestServer,
    project_id: &str,
    title: &str,
) -> serde_json::Value {
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/tasks",
            &json!({"project_id": project_id, "title": title}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "create_task failed");
    helpers::body_json(resp).await["task"].clone()
}

async fn claim(srv: &helpers::TestServer, id: &str, agent: &str) -> axum::http::Response<Body> {
    helpers::call(
        &srv.app,
        helpers::put_json(&format!("/api/tasks/{id}/claim"), &json!({"agent": agent})),
    )
    .await
}

// ── Create ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_task_ok() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({"project_id": "proj-1", "title": "Do something"}),
        ),
    )
    .await;
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
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_create_task_missing_title() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json("/api/tasks", &json!({"project_id": "p1"})),
    )
    .await;
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
    let req = Request::builder()
        .method("GET")
        .uri("/api/tasks")
        .body(Body::empty())
        .unwrap();
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
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_claim_nonexistent_task_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::put_json("/api/tasks/no-such-id/claim", &json!({"agent": "agent-a"})),
    )
    .await;
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
        helpers::put_json(
            &format!("/api/tasks/{id}/unclaim"),
            &json!({"agent": "agent-a"}),
        ),
    )
    .await;
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
        helpers::put_json(
            &format!("/api/tasks/{id}/complete"),
            &json!({"agent": "agent-a"}),
        ),
    )
    .await;
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
        helpers::put_json(
            &format!("/api/tasks/{id}/complete"),
            &json!({"agent": "agent-a"}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        helpers::body_json(resp).await["task"]["status"],
        "completed"
    );
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
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(helpers::body_json(resp).await["task"]["title"], "New title");
}

// ── Schema v4 columns ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_schema_v4_columns_exist() {
    use acc_server::db;
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    db::open_fleet(":memory:").unwrap(); // triggers init_schema + run_migrations

    // Use the test server's fleet_db (already migrated)
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/tasks")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // If new columns exist, the list endpoint should work without error
    let body = helpers::body_json(resp).await;
    assert!(body.get("tasks").is_some());
}

// ── Task type and phase ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_task_with_task_type_and_phase() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "proj-1",
                "title": "Review something",
                "task_type": "review",
                "phase": "alpha",
                "review_of": "task-original-abc",
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["task"]["task_type"], "review");
    assert_eq!(body["task"]["phase"], "alpha");
    assert_eq!(body["task"]["review_of"], "task-original-abc");
}

#[tokio::test]
async fn test_create_task_default_type_is_work() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "proj-1", "Plain task").await;
    assert_eq!(task["task_type"], "work");
}

#[tokio::test]
async fn test_create_task_with_blocked_by() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "proj-1",
                "title": "Phase commit",
                "task_type": "phase_commit",
                "phase": "alpha",
                "blocked_by": ["task-aaa", "task-bbb"],
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    let blocked = body["task"]["blocked_by"].as_array().unwrap();
    assert_eq!(blocked.len(), 2);
}

#[tokio::test]
async fn test_create_task_with_outcome_and_workflow_fields() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "proj-1",
                "title": "Outcome task",
                "outcome_id": "outcome-123",
                "workflow_role": "join",
                "finisher_agent": "natasha",
                "finisher_session": "claude-proj-1"
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["task"]["outcome_id"], "outcome-123");
    assert_eq!(body["task"]["workflow_role"], "join");
    assert_eq!(body["task"]["finisher_agent"], "natasha");
    assert_eq!(body["task"]["finisher_session"], "claude-proj-1");
}

#[tokio::test]
async fn test_create_task_defaults_outcome_id_and_workflow_role() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "proj-1", "Default workflow").await;
    assert_eq!(task["workflow_role"], "work");
    assert_eq!(task["outcome_id"], task["id"]);
}

// ── Filtering ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_tasks_filter_by_task_type() {
    let ts = helpers::TestServer::new().await;
    // Create one of each type
    helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1", "title": "Work task", "task_type": "work",
            }),
        ),
    )
    .await;
    helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1", "title": "Review task", "task_type": "review",
            }),
        ),
    )
    .await;
    helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1", "title": "Phase commit", "task_type": "phase_commit",
            }),
        ),
    )
    .await;

    let resp = helpers::call(&ts.app, helpers::get("/api/tasks?task_type=review")).await;
    let body = helpers::body_json(resp).await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["task_type"], "review");

    let resp2 = helpers::call(&ts.app, helpers::get("/api/tasks?task_type=work")).await;
    let body2 = helpers::body_json(resp2).await;
    assert_eq!(body2["tasks"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_list_tasks_filter_by_phase() {
    let ts = helpers::TestServer::new().await;
    helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1", "title": "Alpha task 1", "phase": "alpha",
            }),
        ),
    )
    .await;
    helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1", "title": "Alpha task 2", "phase": "alpha",
            }),
        ),
    )
    .await;
    helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1", "title": "Beta task", "phase": "beta",
            }),
        ),
    )
    .await;

    let resp = helpers::call(&ts.app, helpers::get("/api/tasks?phase=alpha")).await;
    let tasks = helpers::body_json(resp).await;
    assert_eq!(tasks["tasks"].as_array().unwrap().len(), 2);
}

// ── Dependency blocking (423) ─────────────────────────────────────────────────

async fn set_review_result(srv: &helpers::TestServer, id: &str, result: &str) {
    helpers::call(
        &srv.app,
        helpers::put_json(
            &format!("/api/tasks/{id}/review-result"),
            &json!({"agent":"reviewer","result":result}),
        ),
    )
    .await;
}

#[tokio::test]
async fn test_claim_blocked_task_returns_423() {
    let ts = helpers::TestServer::new().await;
    let blocker = create_task(&ts, "p1", "Blocker task").await;
    let blocker_id = blocker["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1",
                "title": "Blocked task",
                "blocked_by": [blocker_id],
            }),
        ),
    )
    .await;
    let blocked_task = helpers::body_json(resp).await["task"].clone();
    let blocked_id = blocked_task["id"].as_str().unwrap();

    let r = claim(&ts, blocked_id, "agent-a").await;
    assert_eq!(
        r.status(),
        StatusCode::LOCKED,
        "blocked task must return 423"
    );
    let body = helpers::body_json(r).await;
    assert_eq!(body["error"], "blocked");
}

#[tokio::test]
async fn test_claim_unblocks_when_dep_completed_and_approved() {
    let ts = helpers::TestServer::new().await;
    let blocker = create_task(&ts, "p1", "Blocker").await;
    let blocker_id = blocker["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1",
                "title": "Dependent",
                "blocked_by": [blocker_id],
            }),
        ),
    )
    .await;
    let dependent_id = helpers::body_json(resp).await["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Blocker still open → 423
    assert_eq!(
        claim(&ts, &dependent_id, "agent-a").await.status(),
        StatusCode::LOCKED
    );

    // Complete blocker (no review_result = treated as approved)
    helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{blocker_id}/complete"),
            &json!({"agent":"agent-a"}),
        ),
    )
    .await;

    // Now dependent should be claimable
    let r = claim(&ts, &dependent_id, "agent-a").await;
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "should be claimable after blocker completes"
    );
}

#[tokio::test]
async fn test_claim_stays_blocked_on_rejected_review() {
    let ts = helpers::TestServer::new().await;
    let blocker = create_task(&ts, "p1", "Work task").await;
    let blocker_id = blocker["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "p1",
                "title": "Phase commit",
                "blocked_by": [blocker_id],
            }),
        ),
    )
    .await;
    let dependent_id = helpers::body_json(resp).await["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Complete blocker but mark as rejected
    helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{blocker_id}/complete"),
            &json!({"agent":"a"}),
        ),
    )
    .await;
    set_review_result(&ts, blocker_id, "rejected").await;

    // Dependent must still be blocked
    let r = claim(&ts, &dependent_id, "agent-a").await;
    assert_eq!(
        r.status(),
        StatusCode::LOCKED,
        "rejected review must keep task blocked"
    );
}

// ── Review result endpoint ────────────────────────────────────────────────────

#[tokio::test]
async fn test_set_review_result_approved() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Work item").await;
    let id = task["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{id}/review-result"),
            &json!({
                "agent": "reviewer-a",
                "result": "approved",
                "notes": "Looks good to me",
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(helpers::body_json(resp).await["ok"], true);

    // Verify it persists in GET
    let get_resp = helpers::call(&ts.app, helpers::get(&format!("/api/tasks/{id}"))).await;
    let got = helpers::body_json(get_resp).await;
    assert_eq!(got["review_result"], "approved");
    assert_eq!(got["metadata"]["review_notes"], "Looks good to me");
}

#[tokio::test]
async fn test_set_review_result_rejected() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Broken item").await;
    let id = task["id"].as_str().unwrap();

    helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{id}/review-result"),
            &json!({
                "agent": "reviewer-b",
                "result": "rejected",
                "notes": "Tests are missing",
            }),
        ),
    )
    .await;

    let get_resp = helpers::call(&ts.app, helpers::get(&format!("/api/tasks/{id}"))).await;
    assert_eq!(
        helpers::body_json(get_resp).await["review_result"],
        "rejected"
    );
}

#[tokio::test]
async fn test_set_review_result_invalid_value_returns_400() {
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "p1", "Task").await;
    let id = task["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{id}/review-result"),
            &json!({"result":"maybe"}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_set_review_result_unknown_task_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::put_json(
            "/api/tasks/no-such-task/review-result",
            &json!({"result":"approved"}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── DAG: cycle detection ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_create_task_with_valid_blocked_by() {
    let ts = helpers::TestServer::new().await;
    let dep = create_task(&ts, "proj-1", "Dependency task").await;
    let dep_id = dep["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "proj-1",
                "title": "Dependent task",
                "blocked_by": [dep_id]
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["task"]["blocked_by"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_create_task_self_cycle_rejected() {
    // A task can't depend on itself.
    // Since the ID is generated server-side we can't easily pre-specify it,
    // but we CAN update an existing task's blocked_by to point to itself.
    let ts = helpers::TestServer::new().await;
    let task = create_task(&ts, "proj-1", "Self-looping task").await;
    let id = task["id"].as_str().unwrap();

    let resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{id}"), &json!({"blocked_by": [id]})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "self-cycle must be rejected"
    );
}

#[tokio::test]
async fn test_update_task_cycle_in_chain_rejected() {
    let ts = helpers::TestServer::new().await;
    let a = create_task(&ts, "proj-1", "Task A").await;
    let b = create_task(&ts, "proj-1", "Task B").await;
    let aid = a["id"].as_str().unwrap();
    let bid = b["id"].as_str().unwrap();

    // Make B depend on A
    let resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{bid}"), &json!({"blocked_by": [aid]})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Now try to make A depend on B — would create A→B→A cycle
    let resp = helpers::call(
        &ts.app,
        helpers::put_json(&format!("/api/tasks/{aid}"), &json!({"blocked_by": [bid]})),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "A→B→A cycle must be rejected"
    );
}

// ── DAG: auto-unblock on complete ─────────────────────────────────────────────

#[tokio::test]
async fn test_complete_task_unblocks_dependents() {
    let ts = helpers::TestServer::new().await;
    let blocker = create_task(&ts, "proj-dag", "Blocker").await;
    let blocker_id = blocker["id"].as_str().unwrap().to_string();

    // Create a task blocked by blocker
    let dep_resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "proj-dag",
                "title": "Waiting task",
                "blocked_by": [&blocker_id]
            }),
        ),
    )
    .await;
    let dep_id = helpers::body_json(dep_resp).await["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Claim the blocker
    helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{blocker_id}/claim"),
            &json!({"agent":"bob"}),
        ),
    )
    .await;

    // Complete the blocker
    helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{blocker_id}/complete"),
            &json!({"agent":"bob"}),
        ),
    )
    .await;

    // Verify dep is now claimable (no longer blocked)
    let claim_resp = helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{dep_id}/claim"),
            &json!({"agent":"alice"}),
        ),
    )
    .await;
    assert_eq!(
        claim_resp.status(),
        StatusCode::OK,
        "dependent must be claimable after blocker completes"
    );
}

// ── GET /api/tasks/graph ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_task_graph_empty() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/tasks/graph")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["node_count"], 0);
    assert_eq!(body["edge_count"], 0);
    assert!(body["nodes"].as_array().unwrap().is_empty());
    assert!(body["edges"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_get_task_graph_with_dependency_edge() {
    let ts = helpers::TestServer::new().await;
    let a = create_task(&ts, "proj-g", "Task A").await;
    let aid = a["id"].as_str().unwrap();

    // Create B depending on A
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "proj-g",
                "title": "Task B",
                "blocked_by": [aid]
            }),
        ),
    )
    .await;
    let bid = helpers::body_json(resp).await["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let graph =
        helpers::body_json(helpers::call(&ts.app, helpers::get("/api/tasks/graph")).await).await;

    assert_eq!(graph["node_count"], 2);
    assert_eq!(graph["edge_count"], 1);
    let edges = graph["edges"].as_array().unwrap();
    assert!(edges.iter().any(|e| e["from"] == bid && e["to"] == aid));
}

// ── POST /api/tasks/:id/fanout ────────────────────────────────────────────────

#[tokio::test]
async fn test_fanout_creates_children_and_join_gate() {
    let ts = helpers::TestServer::new().await;
    let parent = create_task(&ts, "proj-f", "Parent task").await;
    let pid = parent["id"].as_str().unwrap().to_string();

    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            &format!("/api/tasks/{pid}/fanout"),
            &json!({
                "tasks": [
                    {"title": "Child 1", "description": "first child"},
                    {"title": "Child 2", "description": "second child"}
                ]
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["children"].as_array().unwrap().len(), 2);

    // Parent should now be a join gate blocked by both children
    let parent_resp = helpers::call(&ts.app, helpers::get(&format!("/api/tasks/{pid}"))).await;
    let parent_task = helpers::body_json(parent_resp).await;
    let blocked_by = parent_task["blocked_by"].as_array().unwrap();
    assert_eq!(
        blocked_by.len(),
        2,
        "parent must be blocked by both children"
    );

    // Children should be independently claimable
    let child_id = body["children"][0].as_str().unwrap().to_string();
    let claim_resp = helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{child_id}/claim"),
            &json!({"agent":"worker"}),
        ),
    )
    .await;
    assert_eq!(
        claim_resp.status(),
        StatusCode::OK,
        "child task must be claimable"
    );
}

#[tokio::test]
async fn test_fanout_parent_unblocks_when_all_children_complete() {
    let ts = helpers::TestServer::new().await;
    let parent = create_task(&ts, "proj-f2", "Join parent").await;
    let pid = parent["id"].as_str().unwrap().to_string();

    let fanout = helpers::body_json(
        helpers::call(
            &ts.app,
            helpers::post_json(
                &format!("/api/tasks/{pid}/fanout"),
                &json!({
                    "tasks": [{"title": "Child A"}, {"title": "Child B"}]
                }),
            ),
        )
        .await,
    )
    .await;
    let children = fanout["children"].as_array().unwrap().clone();
    let c1 = children[0].as_str().unwrap().to_string();
    let c2 = children[1].as_str().unwrap().to_string();

    // Complete both children
    for cid in [&c1, &c2] {
        helpers::call(
            &ts.app,
            helpers::put_json(&format!("/api/tasks/{cid}/claim"), &json!({"agent":"w"})),
        )
        .await;
        helpers::call(
            &ts.app,
            helpers::put_json(&format!("/api/tasks/{cid}/complete"), &json!({"agent":"w"})),
        )
        .await;
    }

    // Parent join gate should now be claimable
    let claim_resp = helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{pid}/claim"),
            &json!({"agent":"joiner"}),
        ),
    )
    .await;
    assert_eq!(
        claim_resp.status(),
        StatusCode::OK,
        "join gate must be claimable after all children complete"
    );
}

#[tokio::test]
async fn test_fanout_unknown_parent_returns_404() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks/no-such/fanout",
            &json!({"tasks":[{"title":"x"}]}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_fanout_empty_tasks_returns_400() {
    let ts = helpers::TestServer::new().await;
    let parent = create_task(&ts, "proj-f3", "Parent").await;
    let pid = parent["id"].as_str().unwrap();
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(&format!("/api/tasks/{pid}/fanout"), &json!({"tasks":[]})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
