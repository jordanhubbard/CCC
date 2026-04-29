mod helpers;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

fn no_auth_get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn test_chains_require_auth() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/chains")).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_create_and_get_chain() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/chains",
            &json!({
                "id": "chain-test-1",
                "source": "slack",
                "workspace": "omgjkh",
                "channel_id": "C123",
                "thread_id": "1710000000.000100",
                "title": "Investigate build failure",
                "participants": [{"id": "U1", "platform": "slack", "name": "Ada"}],
                "entities": [{"type": "project", "id": "proj-a", "label": "Project A"}]
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = helpers::call(&ts.app, helpers::get("/api/chains/chain-test-1")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["id"], "chain-test-1");
    assert_eq!(body["source"], "slack");
    assert_eq!(body["participants"][0]["id"], "U1");
    assert_eq!(body["entities"][0]["type"], "project");
}

#[tokio::test]
async fn test_append_event_indexes_participant_and_entity() {
    let ts = helpers::TestServer::new().await;
    let _ = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/chains",
            &json!({"id": "chain-test-2", "source": "telegram", "channel_id": "42"}),
        ),
    )
    .await;

    let resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/chains/chain-test-2/events",
            &json!({
                "event_type": "message",
                "source_event_id": "42:7",
                "actor_id": "1001",
                "actor_name": "Grace",
                "text": "Please fix this failure",
                "entities": [{"type": "error", "id": "E_BUILD", "label": "build failure"}]
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["event"]["event_type"], "message");
    assert_eq!(body["chain"]["title"], "Please fix this failure");
    assert_eq!(body["chain"]["participants"][0]["id"], "1001");
    assert_eq!(body["chain"]["entities"][0]["type"], "error");
}

#[tokio::test]
async fn test_append_duplicate_source_event_is_idempotent() {
    let ts = helpers::TestServer::new().await;
    let event = json!({
        "event_type": "message",
        "source": "slack",
        "source_event_id": "C1:171",
        "actor_id": "U1",
        "text": "same event"
    });

    for _ in 0..2 {
        let resp = helpers::call(
            &ts.app,
            helpers::post_json("/api/chains/chain-test-dup/events", &event),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let resp = helpers::call(&ts.app, helpers::get("/api/chains/chain-test-dup")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_task_create_and_complete_updates_chain_task_link() {
    let ts = helpers::TestServer::new().await;
    let _ = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/chains",
            &json!({"id": "chain-test-task", "source": "slack"}),
        ),
    )
    .await;

    let create_resp = helpers::call(
        &ts.app,
        helpers::post_json(
            "/api/tasks",
            &json!({
                "project_id": "proj-chain",
                "title": "Fix chain-linked task",
                "chain_id": "chain-test-task"
            }),
        ),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let task_id = helpers::body_json(create_resp).await["task"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let chain = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/chains/chain-test-task")).await,
    )
    .await;
    assert_eq!(chain["tasks"][0]["task_id"], task_id);
    assert_eq!(chain["tasks"][0]["status"], "open");

    let complete_resp = helpers::call(
        &ts.app,
        helpers::put_json(
            &format!("/api/tasks/{task_id}/complete"),
            &json!({"agent": "agent-a", "output": "done"}),
        ),
    )
    .await;
    assert_eq!(complete_resp.status(), StatusCode::OK);

    let chain = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/chains/chain-test-task")).await,
    )
    .await;
    assert_eq!(chain["tasks"][0]["status"], "completed");
    assert_eq!(
        chain["tasks"][0]["metadata"]["last_task_status"],
        "completed"
    );
    assert!(chain["tasks"][0]["resolved_at"].as_str().is_some());
}
