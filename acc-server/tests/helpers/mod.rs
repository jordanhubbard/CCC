use acc_server::{brain, build_app, db, state, AppState};
use axum::{
    body::{Body, Bytes},
    http::{Request, Response},
    Router,
};
use std::collections::HashSet;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::RwLock;

pub const TEST_TOKEN: &str = "test-integration-token-abc123";

pub struct TestServer {
    pub app: Router,
    pub token: &'static str,
    pub tmp: TempDir,
}

impl TestServer {
    pub async fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = make_state(&tmp).await;
        let app = build_app(state);
        TestServer { app, token: TEST_TOKEN, tmp }
    }

    pub fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

pub async fn make_state(tmp: &TempDir) -> Arc<AppState> {
    let dir = tmp.path();

    let auth_conn = db::open_auth(":memory:").expect("open auth db");
    let initial_hashes: HashSet<String> = db::auth_all_token_hashes(&auth_conn)
        .into_iter()
        .collect();
    let auth_db = Arc::new(tokio::sync::Mutex::new(auth_conn));

    let fleet_db = db::open_fleet(":memory:").expect("open fleet db");
    let fleet_db = Arc::new(tokio::sync::Mutex::new(fleet_db));

    Arc::new(AppState {
        auth_tokens: HashSet::from([TEST_TOKEN.to_string()]),
        user_token_hashes: std::sync::RwLock::new(initial_hashes),
        auth_db,
        fleet_db,
        queue_path:    dir.join("queue.json").to_string_lossy().into_owned(),
        agents_path:   dir.join("agents.json").to_string_lossy().into_owned(),
        secrets_path:  dir.join("secrets.json").to_string_lossy().into_owned(),
        bus_log_path:  dir.join("bus.jsonl").to_string_lossy().into_owned(),
        projects_path: dir.join("projects.json").to_string_lossy().into_owned(),
        queue:    RwLock::new(state::QueueData::default()),
        agents:   RwLock::new(serde_json::Value::Object(serde_json::Map::new())),
        secrets:  RwLock::new(serde_json::Map::new()),
        projects: tokio::sync::RwLock::new(Vec::new()),
        brain:    Arc::new(brain::BrainQueue::new()),
        bus_tx:   tokio::sync::broadcast::channel(256).0,
        bus_seq:  std::sync::atomic::AtomicU64::new(0),
        start_time: std::time::SystemTime::now(),
        fs_root:  dir.join("fs").to_string_lossy().into_owned(),
        supervisor: None,
        soul_store: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        blob_store: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        blobs_path: dir.join("blobs").to_string_lossy().into_owned(),
        dlq_path:   dir.join("bus-dlq.jsonl").to_string_lossy().into_owned(),
        user_token_roles: std::sync::RwLock::new(std::collections::HashMap::new()),
        watchdog: acc_server::routes::watchdog::WatchdogState::new(),
    })
}

/// Convenience: send a request through the router without binding a port.
pub async fn call(app: &Router, req: Request<Body>) -> Response<Body> {
    use tower::ServiceExt;
    app.clone().oneshot(req).await.expect("handler panicked")
}

/// Read response body bytes.
pub async fn body_bytes(resp: Response<Body>) -> Bytes {
    use http_body_util::BodyExt;
    resp.into_body().collect().await.expect("body read").to_bytes()
}

/// Read response body as JSON.
pub async fn body_json(resp: Response<Body>) -> serde_json::Value {
    let bytes = body_bytes(resp).await;
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Build a GET request with auth token.
pub fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("Authorization", format!("Bearer {}", TEST_TOKEN))
        .body(Body::empty())
        .unwrap()
}

/// Build a POST request with JSON body and auth token.
pub fn post_json(path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("Authorization", format!("Bearer {}", TEST_TOKEN))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Build a PATCH request with JSON body and auth token.
pub fn patch_json(path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(path)
        .header("Authorization", format!("Bearer {}", TEST_TOKEN))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Build a PUT request with JSON body and auth token.
pub fn put_json(path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(path)
        .header("Authorization", format!("Bearer {}", TEST_TOKEN))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Build a DELETE request with auth token.
pub fn delete(path: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(path)
        .header("Authorization", format!("Bearer {}", TEST_TOKEN))
        .body(Body::empty())
        .unwrap()
}
