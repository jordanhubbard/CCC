mod helpers;
use acc_server::build_app;
use helpers::{body_json, call, get, make_state, post_json, TestServer};
use serde_json::json;

async fn server_with_agents() -> TestServer {
    let srv = TestServer::new().await;
    // Register two agents
    call(
        &srv.app,
        post_json(
            "/api/agents/register",
            &json!({
                "name": "boris", "host": "boris-host.local",
                "capabilities": ["gpu", "inference"], "token": "tok-boris"
            }),
        ),
    )
    .await;
    call(
        &srv.app,
        post_json(
            "/api/agents/register",
            &json!({
                "name": "ollama", "host": "ollama-server.hrd.nvidia.com",
                "capabilities": ["ollama", "general"], "token": "tok-ollama"
            }),
        ),
    )
    .await;
    srv
}

#[tokio::test]
async fn test_get_soul_returns_registry() {
    let srv = server_with_agents().await;
    let resp = call(&srv.app, get("/api/agents/boris/soul")).await;
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["soul"]["agent"], json!("boris"));
    assert!(body["soul"]["registry"].is_object());
    assert_eq!(body["soul"]["host_data_status"], json!("pending"));
}

#[tokio::test]
async fn test_get_soul_unknown_agent_returns_404() {
    let srv = TestServer::new().await;
    let resp = call(&srv.app, get("/api/agents/nobody/soul")).await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_post_soul_data_stores_and_retrieves() {
    let srv = server_with_agents().await;

    // Agent uploads its host data
    let upload = post_json(
        "/api/agents/boris/soul/data",
        &json!({
            "agent": "boris",
            "tar_gz_hex": "deadbeef",
            "exported_at": "2026-04-21T00:00:00Z",
            "size_bytes": 4
        }),
    );
    let resp = call(&srv.app, upload).await;
    assert_eq!(resp.status(), 200);

    // GET /soul now includes host_data
    let resp = call(&srv.app, get("/api/agents/boris/soul")).await;
    let body = body_json(resp).await;
    assert_eq!(body["soul"]["host_data_status"], json!("ready"));
    assert_eq!(body["soul"]["host_data"]["tar_gz_hex"], json!("deadbeef"));
}

#[tokio::test]
async fn test_move_agent_merges_identity_onto_target() {
    let srv = server_with_agents().await;

    let resp = call(
        &srv.app,
        post_json(
            "/api/agents/move",
            &json!({
                "source": "boris",
                "target": "ollama",
                "decommission_source": true
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["source"], json!("boris"));
    assert_eq!(body["target"], json!("ollama"));

    // "ollama" entry should be gone; "boris" entry should have ollama's host
    let agents_resp = call(&srv.app, get("/api/agents")).await;
    let agents_body = body_json(agents_resp).await;
    let agents = agents_body["agents"].as_array().unwrap();
    let names: Vec<&str> = agents.iter().filter_map(|a| a["name"].as_str()).collect();
    assert!(names.contains(&"boris"), "boris should exist");
    assert!(!names.contains(&"ollama"), "ollama should be gone");

    let boris = agents.iter().find(|a| a["name"] == "boris").unwrap();
    assert_eq!(
        boris["host"],
        json!("ollama-server.hrd.nvidia.com"),
        "should have ollama's host"
    );
}

#[tokio::test]
async fn test_move_agent_unknown_source_returns_404() {
    let srv = server_with_agents().await;
    let resp = call(
        &srv.app,
        post_json(
            "/api/agents/move",
            &json!({
                "source": "nobody", "target": "ollama"
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_move_agent_same_source_target_returns_400() {
    let srv = server_with_agents().await;
    let resp = call(
        &srv.app,
        post_json(
            "/api/agents/move",
            &json!({
                "source": "boris", "target": "boris"
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), 400);
}
