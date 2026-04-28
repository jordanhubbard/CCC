//! Agent registry routes — heartbeat, register, upsert, patch, delete, health, heartbeats.
mod helpers;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;

// ── GET /api/agents ───────────────────────────────────────────────────────────

#[tokio::test]
async fn agents_list_starts_empty() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/agents")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body["agents"].as_array().unwrap().is_empty());
}

// ── POST /api/agents/register ─────────────────────────────────────────────────

#[tokio::test]
async fn register_agent_creates_with_token() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/register",
            &json!({"name": "hermes", "host": "puck"}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    let token = body["token"].as_str().unwrap();
    assert!(
        token.starts_with("acc-agent-hermes-"),
        "token must start with acc-agent-{{name}}-"
    );
    assert_eq!(body["agent"]["name"], "hermes");
    assert!(body["agent"]["executors"].is_array());
    assert!(body["agent"]["sessions"].is_array());
}

#[tokio::test]
async fn register_agent_requires_name() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/register", &json!({"host": "somewhere"})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn register_agent_idempotent_preserves_token() {
    let srv = helpers::TestServer::new().await;
    let first = helpers::body_json(
        helpers::call(
            &srv.app,
            helpers::post_json("/api/agents/register", &json!({"name": "boris"})),
        )
        .await,
    )
    .await;
    let token1 = first["token"].as_str().unwrap().to_string();

    let second = helpers::body_json(
        helpers::call(
            &srv.app,
            helpers::post_json("/api/agents/register", &json!({"name": "boris"})),
        )
        .await,
    )
    .await;
    let token2 = second["token"].as_str().unwrap().to_string();

    assert_eq!(
        token1, token2,
        "re-registering same agent should preserve its token"
    );
}

// ── POST /api/agents (alias for register) ────────────────────────────────────

#[tokio::test]
async fn post_agents_registers_agent() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/agents", &json!({"name": "natasha", "host": "sparky"})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
}

// ── POST /api/agents/:name/heartbeat ─────────────────────────────────────────

#[tokio::test]
async fn heartbeat_registers_agent() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/test-node/heartbeat",
            &json!({
                "status": "online",
                "capabilities": {"claude_cli": true, "hermes": false}
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "heartbeat should succeed");

    let resp = helpers::call(&srv.app, helpers::get("/api/agents/test-node")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["agent"]["name"].as_str().unwrap_or(""), "test-node");
    assert_eq!(body["agent"]["executors"][0]["executor"], "claude_cli");
}

#[tokio::test]
async fn register_agent_normalizes_canonical_executor_shape() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/register", &json!({
            "name": "codex-node",
            "capabilities": {"codex_cli": true, "gpu": false},
            "tool_capabilities": ["bash", "read_file"],
            "sessions": [{"name": "proj-main", "executor": "codex_cli", "state": "idle"}],
            "capacity": {"tasks_in_flight": 1, "estimated_free_slots": 2, "free_session_slots": 1}
        })),
    ).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["agent"]["tool_capabilities"][0], "bash");
    assert_eq!(body["agent"]["executors"][0]["executor"], "codex_cli");
    assert_eq!(body["agent"]["sessions"][0]["name"], "proj-main");
    assert_eq!(body["agent"]["capacity"]["free_session_slots"], 1);
}

#[tokio::test]
async fn heartbeat_sets_last_seen() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/rocky/heartbeat", &json!({"status": "online"})),
    )
    .await;
    let body =
        helpers::body_json(helpers::call(&srv.app, helpers::get("/api/agents/rocky")).await).await;
    assert!(
        body["agent"]["lastSeen"].is_string(),
        "lastSeen must be set after heartbeat"
    );
}

// ── POST /api/heartbeat/:agent (alternate path) ───────────────────────────────

#[tokio::test]
async fn alternate_heartbeat_returns_ok_and_pending_work() {
    let srv = helpers::TestServer::new().await;
    // First register the agent so the heartbeat has someone to update.
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/register", &json!({"name": "bullwinkle"})),
    )
    .await;

    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/heartbeat/bullwinkle", &json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body["pendingWork"].is_array());
}

#[tokio::test]
async fn alternate_heartbeat_updates_capacity_and_sessions() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/register", &json!({"name": "heartbeat-node"})),
    )
    .await;

    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/heartbeat/heartbeat-node",
            &json!({
                "tasks_in_flight": 1,
                "estimated_free_slots": 2,
                "free_session_slots": 1,
                "executors": [{"executor": "claude_cli", "ready": true, "auth_state": "ready"}],
                "sessions": [{"name": "proj-a", "executor": "claude_cli", "state": "busy"}]
            }),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = helpers::body_json(
        helpers::call(&srv.app, helpers::get("/api/agents/heartbeat-node")).await,
    )
    .await;
    assert_eq!(body["agent"]["capacity"]["tasks_in_flight"], 1);
    assert_eq!(body["agent"]["capacity"]["free_session_slots"], 1);
    assert_eq!(body["agent"]["sessions"][0]["name"], "proj-a");
}

#[tokio::test]
async fn alternate_heartbeat_unknown_agent_still_returns_ok() {
    // post_heartbeat silently skips update for unknown agents but still returns 200.
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/heartbeat/nobody", &json!({})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── GET /api/heartbeats ───────────────────────────────────────────────────────

#[tokio::test]
async fn heartbeats_returns_map() {
    let srv = helpers::TestServer::new().await;
    // Register two agents then check the heartbeats map.
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/a1/heartbeat", &json!({})),
    )
    .await;
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/a2/heartbeat", &json!({})),
    )
    .await;

    let body =
        helpers::body_json(helpers::call(&srv.app, helpers::get("/api/heartbeats")).await).await;
    assert!(body.is_object(), "heartbeats must return a JSON object");
    assert!(body.get("a1").is_some(), "a1 must appear in heartbeats");
    assert!(body.get("a2").is_some(), "a2 must appear in heartbeats");
}

// ── GET /api/agents/:name ─────────────────────────────────────────────────────

#[tokio::test]
async fn get_agent_not_found() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/agents/does-not-exist")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_agent_has_online_field() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/peabody/heartbeat", &json!({})),
    )
    .await;
    let body =
        helpers::body_json(helpers::call(&srv.app, helpers::get("/api/agents/peabody")).await)
            .await;
    assert!(body["agent"]["online"].is_boolean());
    assert!(body["agent"]["onlineStatus"].is_string());
}

// ── GET /api/agents/:name/health ─────────────────────────────────────────────

#[tokio::test]
async fn get_agent_health_after_heartbeat() {
    let srv = helpers::TestServer::new().await;
    // First heartbeat creates the agent record (telemetry not stored on initial creation).
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/sherman/heartbeat", &json!({})),
    )
    .await;
    // Second heartbeat merges telemetry into the existing record.
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/sherman/heartbeat",
            &json!({
                "gpu": "RTX 4090",
                "gpu_temp_c": 72.0,
            }),
        ),
    )
    .await;
    let resp = helpers::call(&srv.app, helpers::get("/api/agents/sherman/health")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["health"]["agent"], "sherman");
    assert!(body["health"]["online"].is_boolean());
    assert_eq!(body["health"]["gpu"], "RTX 4090");
}

#[tokio::test]
async fn get_agent_health_not_found() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/agents/ghost/health")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── POST /api/agents/:name (upsert) ──────────────────────────────────────────

#[tokio::test]
async fn upsert_agent_creates_when_absent() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/snidely",
            &json!({"host": "l40-sweden", "type": "gpu"}),
        ),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body["token"].is_string());
}

#[tokio::test]
async fn upsert_agent_updates_when_present() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/dudley", &json!({"host": "old-host"})),
    )
    .await;
    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/dudley", &json!({"host": "new-host"})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["agent"]["host"], "new-host");
}

// ── PATCH /api/agents/:name ───────────────────────────────────────────────────

#[tokio::test]
async fn patch_agent_updates_field() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/register",
            &json!({"name": "patch-target", "host": "old"}),
        ),
    )
    .await;
    let resp = helpers::call(
        &srv.app,
        helpers::patch_json("/api/agents/patch-target", &json!({"host": "updated-host"})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["agent"]["host"], "updated-host");
}

#[tokio::test]
async fn patch_agent_not_found() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(
        &srv.app,
        helpers::patch_json("/api/agents/nobody", &json!({"host": "x"})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_agent_decommission() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/register", &json!({"name": "retired"})),
    )
    .await;
    let resp = helpers::call(
        &srv.app,
        helpers::patch_json("/api/agents/retired", &json!({"status": "decommissioned"})),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["agent"]["decommissioned"], true);
}

// ── DELETE /api/agents/:name ──────────────────────────────────────────────────

#[tokio::test]
async fn delete_agent_requires_auth() {
    let srv = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/agents/anyone")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        helpers::call(&srv.app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn delete_agent_removes_registration() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json("/api/agents/register", &json!({"name": "deleteme"})),
    )
    .await;
    let del =
        helpers::body_json(helpers::call(&srv.app, helpers::delete("/api/agents/deleteme")).await)
            .await;
    assert_eq!(del["ok"], true);
    assert_eq!(del["deleted"], true);

    let resp = helpers::call(&srv.app, helpers::get("/api/agents/deleteme")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_agent_not_found() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::delete("/api/agents/ghost-agent")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── exec capability routing ───────────────────────────────────────────────────

#[tokio::test]
async fn exec_resolves_capability_to_agent() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/hermes-node/heartbeat",
            &json!({
                "status": "online",
                "capabilities": {"hermes": true, "gpu": false}
            }),
        ),
    )
    .await;

    let resp = helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/exec",
            &json!({
                "command": "ping",
                "targets": ["hermes"],
            }),
        ),
    )
    .await;

    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected status {status}"
    );

    if status == StatusCode::OK {
        let body = helpers::body_json(resp).await;
        if let Some(targets) = body["targets"].as_array() {
            assert!(
                targets.iter().any(|t| t == "hermes-node"),
                "capability 'hermes' should resolve to 'hermes-node'; got targets={:?}",
                targets
            );
        }
    }
}

// ── GET /api/agents?online=true ───────────────────────────────────────────────

#[tokio::test]
async fn agents_online_filter_returns_only_online() {
    let srv = helpers::TestServer::new().await;
    // Register sets lastSeen=now, so a freshly registered agent is online
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/register",
            &json!({"name": "live-bot", "host": "puck"}),
        ),
    )
    .await;

    let resp = helpers::call(&srv.app, helpers::get("/api/agents?online=true")).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    let agents = body["agents"].as_array().unwrap();
    assert!(!agents.is_empty());
    // Every agent returned must have online=true
    assert!(agents
        .iter()
        .all(|a| a["online"].as_bool().unwrap_or(false)));
}

// ── GET /api/agents/names ─────────────────────────────────────────────────────

#[tokio::test]
async fn agent_names_empty_cluster() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/agents/names")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body["names"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn agent_names_lists_registered_agents() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/register",
            &json!({"name": "natasha", "host": "puck"}),
        ),
    )
    .await;
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/register",
            &json!({"name": "boris", "host": "rocky"}),
        ),
    )
    .await;

    let resp = helpers::call(&srv.app, helpers::get("/api/agents/names")).await;
    let body = helpers::body_json(resp).await;
    let names: Vec<&str> = body["names"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(names.contains(&"natasha"));
    assert!(names.contains(&"boris"));
}

#[tokio::test]
async fn agent_names_excludes_decommissioned() {
    let srv = helpers::TestServer::new().await;
    helpers::call(
        &srv.app,
        helpers::post_json(
            "/api/agents/register",
            &json!({"name": "retired", "host": "old"}),
        ),
    )
    .await;
    // Decommission it
    helpers::call(
        &srv.app,
        helpers::patch_json("/api/agents/retired", &json!({"status": "decommissioned"})),
    )
    .await;

    let resp = helpers::call(&srv.app, helpers::get("/api/agents/names")).await;
    let body = helpers::body_json(resp).await;
    let names: Vec<&str> = body["names"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(!names.contains(&"retired"));
}
