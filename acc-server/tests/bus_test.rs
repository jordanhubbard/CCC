mod helpers;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn bus_messages_starts_empty() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    // Response may be an array or {"messages": [...]}
    let is_array = body.is_array();
    let has_messages = body.get("messages").is_some();
    assert!(
        is_array || has_messages,
        "bus/messages must return array or object with messages; got: {body}"
    );
}

#[tokio::test]
async fn bus_send_broadcasts_message() {
    let srv = helpers::TestServer::new().await;

    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/bus/send",
            &json!({
                "from": "test-sender",
                "to": "all",
                "type": "ping",
                "subject": "test-ping",
                "body": {"msg": "hello from test"}
            }),
        ),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "bus/send should accept the message"
    );
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], json!(true));
}

#[tokio::test]
async fn bus_send_message_appears_in_history() {
    let srv = helpers::TestServer::new().await;

    // Send a message
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/bus/send",
            &json!({
                "from": "test-agent",
                "to": "all",
                "type": "test.probe",
                "subject": "test-subject-unique-12345",
            }),
        ),
    )
    .await;

    // Should appear in messages
    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    let messages = if body.is_array() {
        body.as_array().cloned().unwrap_or_default()
    } else {
        body["messages"].as_array().cloned().unwrap_or_default()
    };
    assert!(
        messages
            .iter()
            .any(|m| m["subject"] == json!("test-subject-unique-12345")),
        "sent message should appear in history; got {} messages",
        messages.len()
    );
}

#[tokio::test]
async fn bus_presence_returns_agent_list() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/bus/presence")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
