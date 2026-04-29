//! Integration tests that spin up a wiremock server and exercise the client
//! against canned responses that mirror the real server's wire format.

use acc_client::{model::TaskStatus, Client, Error};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_task(id: &str, status: &str) -> serde_json::Value {
    json!({
        "id": id,
        "project_id": "proj-a",
        "title": "t",
        "description": "",
        "status": status,
        "priority": 2,
        "created_at": "2026-04-23T00:00:00Z",
        "task_type": "work",
        "metadata": {},
        "blocked_by": []
    })
}

async fn client_for(server: &MockServer) -> Client {
    Client::new(server.uri(), "test-token").unwrap()
}

#[tokio::test]
async fn list_tasks_filters_by_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tasks"))
        .and(query_param("status", "open"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tasks": [sample_task("task-1", "open")]
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let tasks = client
        .tasks()
        .list()
        .status(TaskStatus::Open)
        .send()
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, "task-1");
    assert_eq!(tasks[0].status, TaskStatus::Open);
}

#[tokio::test]
async fn claim_conflict_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/tasks/task-9/claim"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "error": "already_claimed"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client.tasks().claim("task-9", "agent-a").await.unwrap_err();
    match err {
        Error::Conflict(body) => assert_eq!(body.error, "already_claimed"),
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn claim_locked_preserves_pending_field() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/tasks/task-9/claim"))
        .respond_with(ResponseTemplate::new(423).set_body_json(json!({
            "error": "blocked",
            "pending": "task-1"
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client.tasks().claim("task-9", "a").await.unwrap_err();
    match err {
        Error::Locked(body) => {
            assert_eq!(body.error, "blocked");
            assert_eq!(
                body.extra.get("pending").and_then(|v| v.as_str()),
                Some("task-1")
            );
        }
        other => panic!("expected Locked, got {other:?}"),
    }
}

#[tokio::test]
async fn claim_returns_task_from_wrapped_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/tasks/task-1/claim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task": sample_task("task-1", "claimed")
        })))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let t = client.tasks().claim("task-1", "agent-a").await.unwrap();
    assert_eq!(t.status, TaskStatus::Claimed);
}

#[tokio::test]
async fn get_accepts_bare_task_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tasks/task-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sample_task("task-1", "open")))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let t = client.tasks().get("task-1").await.unwrap();
    assert_eq!(t.id, "task-1");
}

#[tokio::test]
async fn complete_with_non_json_body_still_errors_gracefully() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/tasks/task-1/complete"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let err = client
        .tasks()
        .complete("task-1", Some("a"), None)
        .await
        .unwrap_err();
    match err {
        Error::Api { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body.error, "http_500");
        }
        other => panic!("expected Api{{500}}, got {other:?}"),
    }
}
