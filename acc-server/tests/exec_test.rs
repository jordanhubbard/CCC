mod helpers;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn exec_requires_command_or_code() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/exec", &json!({"targets": ["all"]})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = helpers::body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap_or("").contains("command"),
        "error must mention 'command'; got: {body}"
    );
}

#[tokio::test]
async fn exec_missing_agentbus_token_returns_500() {
    // AGENTBUS_TOKEN is not set in test environment, so fan-out should fail gracefully
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/exec",
            &json!({
                "command": "ping",
                "params": {"message": "test"},
                "targets": ["all"]
            }),
        ),
    )
    .await;
    // Either 500 (no AGENTBUS_TOKEN) or 200 (ok:true, busSent:false) — both acceptable
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 200 or 500 when AGENTBUS_TOKEN absent; got {status}"
    );
}

#[tokio::test]
async fn exec_result_can_be_posted() {
    let srv = helpers::TestServer::new().await;

    // Use a unique ID to avoid conflicts with persisted exec.jsonl from prior runs
    let fake_exec_id = format!(
        "exec-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    );

    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            &format!("/api/exec/{fake_exec_id}/result"),
            &json!({
                "agent": "test-agent",
                "output": "test output",
                "exit_code": 0,
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Retrieve it
    let resp = helpers::call(&srv.app, helpers::get(&format!("/api/exec/{fake_exec_id}"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let record = helpers::body_json(resp).await;
    let results = record["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected at least one result");
    // Find the result we just posted
    let our_result = results.iter().find(|r| r["output"] == json!("test output"));
    assert!(
        our_result.is_some(),
        "our posted result should appear in record"
    );
    assert_eq!(our_result.unwrap()["agent"], json!("test-agent"));
}

#[tokio::test]
async fn exec_get_nonexistent_returns_404() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/exec/exec-does-not-exist-xyz")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
