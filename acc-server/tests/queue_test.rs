mod helpers;

use axum::http::StatusCode;
use serde_json::json;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Create a queue item and return the item object (extracted from {"ok":true,"item":{...}}).
async fn create_item(
    srv: &helpers::TestServer,
    title: &str,
    description: &str,
) -> serde_json::Value {
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/queue",
            &json!({
                "title": title,
                "description": description,
                "_skip_dedup": true,
            }),
        ),
    )
    .await;
    // POST /api/queue returns 201 CREATED
    assert_eq!(resp.status(), StatusCode::CREATED, "create_item failed");
    let body = helpers::body_json(resp).await;
    // Response shape: {"ok": true, "item": {...}}
    body["item"].clone()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn queue_starts_empty() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/queue")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert!(
        body.get("items").is_some(),
        "queue response must have 'items'"
    );
}

#[tokio::test]
async fn create_item_returns_id() {
    let srv = helpers::TestServer::new().await;
    let item = create_item(
        &srv,
        "Test task",
        "This is a test task with enough description",
    )
    .await;
    assert!(item.get("id").is_some(), "item must have 'id'; got: {item}");
}

#[tokio::test]
async fn create_requires_title() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/queue", &json!({"description": "no title here"})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn created_item_is_pending() {
    let srv = helpers::TestServer::new().await;
    let item = create_item(&srv, "Pending test", "A task that should start as pending").await;
    let id = item["id"].as_str().expect("id");

    let resp = helpers::call(&srv.app, helpers::get(&format!("/api/item/{id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let fetched = helpers::body_json(resp).await;
    assert_eq!(
        fetched["status"],
        json!("pending"),
        "new item must be pending; got: {fetched}"
    );
    assert_eq!(fetched["title"], json!("Pending test"));
}

#[tokio::test]
async fn claim_lifecycle() {
    let srv = helpers::TestServer::new().await;

    // Create
    let item = create_item(
        &srv,
        "Claim test",
        "Testing the full claim → complete lifecycle",
    )
    .await;
    let id = item["id"].as_str().expect("id");

    // Claim
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/item/{id}/claim"),
            &json!({"agent": "test-agent"}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "claim should succeed");
    let claim_body = helpers::body_json(resp).await;
    assert_eq!(claim_body["ok"], json!(true));

    // Verify claimed
    let resp = helpers::call(&srv.app, helpers::get(&format!("/api/item/{id}"))).await;
    let fetched = helpers::body_json(resp).await;
    assert_eq!(
        fetched["status"],
        json!("in-progress"),
        "after claim: {fetched}"
    );
    assert_eq!(fetched["claimedBy"], json!("test-agent"));

    // Keepalive
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/item/{id}/keepalive"),
            &json!({"agent": "test-agent"}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "keepalive should succeed");

    // Complete
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/item/{id}/complete"),
            &json!({
                "agent": "test-agent",
                "result": "Task completed successfully in test"
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "complete should succeed");

    // After completion the item should no longer be in active queue
    let resp = helpers::call(&srv.app, helpers::get("/api/queue")).await;
    let body = helpers::body_json(resp).await;
    let items = body["items"].as_array().unwrap();
    assert!(
        !items.iter().any(|i| i["id"] == json!(id)),
        "completed item should not be in active queue"
    );
}

#[tokio::test]
async fn fail_increments_attempts() {
    let srv = helpers::TestServer::new().await;
    let item = create_item(
        &srv,
        "Fail test",
        "This task will fail and check retry logic",
    )
    .await;
    let id = item["id"].as_str().expect("id");

    // Claim
    helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/item/{id}/claim"),
            &json!({"agent": "test-agent"}),
        ),
    )
    .await;

    // Fail
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/item/{id}/fail"),
            &json!({
                "agent": "test-agent",
                "error": "synthetic failure for test"
            }),
        ),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "fail endpoint should return ok"
    );

    // Item should be back to pending with attempts=1
    let resp = helpers::call(&srv.app, helpers::get(&format!("/api/item/{id}"))).await;
    let fetched = helpers::body_json(resp).await;
    assert_eq!(
        fetched["status"],
        json!("pending"),
        "failed item returns to pending; got: {fetched}"
    );
    assert!(
        fetched["attempts"].as_u64().unwrap_or(0) >= 1,
        "attempts must increment"
    );
}

#[tokio::test]
async fn max_attempts_blocks_item() {
    let srv = helpers::TestServer::new().await;
    let item = create_item(
        &srv,
        "Max attempts test",
        "Task that exhausts all retry attempts",
    )
    .await;
    let id = item["id"].as_str().expect("id");

    // Fail maxAttempts times (default is 3)
    for _ in 0..3 {
        helpers::call(
            &srv.app,
            helpers::post_json(
                &format!("/api/item/{id}/claim"),
                &json!({"agent": "test-agent"}),
            ),
        )
        .await;
        helpers::call(
            &srv.app,
            helpers::post_json(
                &format!("/api/item/{id}/fail"),
                &json!({
                    "agent": "test-agent",
                    "error": "forced failure"
                }),
            ),
        )
        .await;
    }

    let resp = helpers::call(&srv.app, helpers::get(&format!("/api/item/{id}"))).await;
    let fetched = helpers::body_json(resp).await;
    assert_eq!(
        fetched["status"],
        json!("blocked"),
        "after maxAttempts, status must be blocked; got: {fetched}"
    );
}

#[tokio::test]
async fn cannot_double_claim() {
    let srv = helpers::TestServer::new().await;
    let item = create_item(
        &srv,
        "Double claim test",
        "No two agents should claim the same task",
    )
    .await;
    let id = item["id"].as_str().expect("id");

    let r1 = helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/item/{id}/claim"),
            &json!({"agent": "agent-a"}),
        ),
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK, "first claim should succeed");

    // Second claim by a different agent should fail
    let r2 = helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/item/{id}/claim"),
            &json!({"agent": "agent-b"}),
        ),
    )
    .await;
    assert!(
        r2.status() == StatusCode::CONFLICT || r2.status() == StatusCode::BAD_REQUEST,
        "double-claim should return 409 or 400; got {}",
        r2.status()
    );
}
