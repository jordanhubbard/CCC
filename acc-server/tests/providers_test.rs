//! Providers routes — no auth required.
//! In the test environment: supervisor is None (disabled), tokenhub unreachable → 503.
mod helpers;

use axum::http::StatusCode;

// ── GET /api/providers ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_providers_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/providers")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_list_providers_returns_providers_array() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/providers")).await,
    ).await;
    assert!(body["providers"].is_array(), "response must have providers array");
    assert!(!body["providers"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_list_providers_each_has_required_fields() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/providers")).await,
    ).await;
    for p in body["providers"].as_array().unwrap() {
        assert!(p["id"].is_string(),      "provider must have id");
        assert!(p["kind"].is_string(),    "provider must have kind");
        assert!(p["label"].is_string(),   "provider must have label");
        assert!(p["status"].is_string(),  "provider must have status");
        assert!(p["enabled"].is_boolean(),"provider must have enabled flag");
    }
}

#[tokio::test]
async fn test_list_providers_includes_tokenhub() {
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/providers")).await,
    ).await;
    let providers = body["providers"].as_array().unwrap();
    assert!(
        providers.iter().any(|p| p["id"] == "tokenhub"),
        "providers must include tokenhub"
    );
}

#[tokio::test]
async fn test_supervisor_disabled_when_none() {
    // TestServer sets supervisor = None → supervisor provider shows status:"disabled", enabled:false.
    let ts = helpers::TestServer::new().await;
    let body = helpers::body_json(
        helpers::call(&ts.app, helpers::get("/api/providers")).await,
    ).await;
    let supervisor = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "supervisor")
        .expect("supervisor provider must be present")
        .clone();
    assert_eq!(supervisor["status"], "disabled");
    assert_eq!(supervisor["enabled"], false);
}

// ── GET /api/providers/models ────────────────────────────────────────────────

#[tokio::test]
async fn test_list_models_no_auth_required() {
    let ts = helpers::TestServer::new().await;
    // Any non-4xx response is acceptable; tokenhub is offline so we expect 503.
    let resp = helpers::call(&ts.app, helpers::get("/api/providers/models")).await;
    assert_ne!(resp.status().as_u16() / 100, 4, "should not be a 4xx — auth is not required");
}

#[tokio::test]
async fn test_list_models_returns_503_when_tokenhub_offline() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::get("/api/providers/models")).await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}
