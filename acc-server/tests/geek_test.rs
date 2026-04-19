//! Geek topology routes — no auth required.
//! /api/geek/topology and /api/mesh are identical (alias).
//! /api/geek/stream (SSE) is not tested — not usable with oneshot dispatch.
mod helpers;

use axum::http::StatusCode;

// ── /api/geek/topology ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_topology_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/geek/topology")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_topology_has_required_fields() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/geek/topology")).await,
    ).await;
    assert!(body["nodes"].is_array(),  "topology must have nodes array");
    assert!(body["edges"].is_array(),  "topology must have edges array");
    assert!(body["heartbeatSummary"].is_array(), "topology must have heartbeatSummary array");
    assert!(body["busMessages"].is_array(), "topology must have busMessages array");
}

#[tokio::test]
async fn test_topology_nodes_non_empty() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/geek/topology")).await,
    ).await;
    let nodes = body["nodes"].as_array().unwrap();
    assert!(!nodes.is_empty(), "static node list must not be empty");
}

#[tokio::test]
async fn test_topology_edges_non_empty() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/geek/topology")).await,
    ).await;
    let edges = body["edges"].as_array().unwrap();
    assert!(!edges.is_empty(), "static edge list must not be empty");
}

#[tokio::test]
async fn test_topology_nodes_have_id_and_type() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/geek/topology")).await,
    ).await;
    for node in body["nodes"].as_array().unwrap() {
        assert!(node["id"].is_string(),   "every node must have an id");
        assert!(node["type"].is_string(), "every node must have a type");
    }
}

#[tokio::test]
async fn test_topology_agent_nodes_have_status() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/geek/topology")).await,
    ).await;
    for node in body["nodes"].as_array().unwrap() {
        if node["type"] == "agent" {
            assert!(node["status"].is_string(), "agent node must have a status field");
        }
    }
}

#[tokio::test]
async fn test_topology_heartbeat_empty_in_fresh_server() {
    // Fresh TestServer has no registered agents → heartbeatSummary is empty.
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/geek/topology")).await,
    ).await;
    assert!(body["heartbeatSummary"].as_array().unwrap().is_empty());
}

// ── /api/mesh (alias) ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_mesh_alias_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/mesh")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_mesh_same_shape_as_topology() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/mesh")).await,
    ).await;
    assert!(body["nodes"].is_array());
    assert!(body["edges"].is_array());
    assert!(body["heartbeatSummary"].is_array());
    assert!(body["busMessages"].is_array());
}
