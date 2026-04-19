//! Lightweight mock ACC hub for agent integration tests.
//!
//! Binds to 127.0.0.1:0 (OS-assigned random port) so parallel tests never conflict.
//! Use HubMock::new() for defaults or HubMock::with_state(HubState{...}).await for custom responses.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, put},
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

// ── State ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HubState {
    /// Items returned by GET /api/queue
    pub queue_items: Vec<Value>,
    /// Tasks returned by GET /api/tasks (filtered by ?status=)
    pub tasks: Vec<Value>,
    /// HTTP status code for POST /api/item/:id/claim  (default 200)
    pub item_claim_status: u16,
    /// HTTP status code for PUT  /api/tasks/:id/claim (default 200)
    pub task_claim_status: u16,
}

impl Default for HubState {
    fn default() -> Self {
        Self {
            queue_items: vec![],
            tasks: vec![],
            item_claim_status: 200,
            task_claim_status: 200,
        }
    }
}

// ── Mock server ───────────────────────────────────────────────────────────────

pub struct HubMock {
    pub url: String,
    pub state: Arc<RwLock<HubState>>,
    _handle: tokio::task::JoinHandle<()>,
}

impl HubMock {
    pub async fn new() -> Self {
        Self::with_state(HubState::default()).await
    }

    pub async fn with_queue(items: Vec<Value>) -> Self {
        Self::with_state(HubState { queue_items: items, ..Default::default() }).await
    }

    pub async fn with_tasks(tasks: Vec<Value>) -> Self {
        Self::with_state(HubState { tasks, ..Default::default() }).await
    }

    pub async fn with_state(initial: HubState) -> Self {
        let state = Arc::new(RwLock::new(initial));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind random port for hub mock");
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{}", addr);
        let app = build_router(state.clone());
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        HubMock { url, state, _handle: handle }
    }
}

impl Drop for HubMock {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

type S = Arc<RwLock<HubState>>;

fn build_router(state: S) -> Router {
    Router::new()
        // Heartbeat — queue worker uses /api/heartbeat/:name
        .route("/api/heartbeat/:name",          post(ok))
        .route("/api/agents/:name/heartbeat",   post(ok))
        // Queue worker item routes
        .route("/api/queue",                    get(queue_items))
        .route("/api/item/:id/claim",           post(item_claim))
        .route("/api/item/:id/complete",        post(ok))
        .route("/api/item/:id/fail",            post(ok))
        .route("/api/item/:id/keepalive",       post(ok))
        .route("/api/item/:id/comment",         post(ok))
        // Fleet task routes
        .route("/api/tasks",                    get(task_list))
        .route("/api/tasks/:id/claim",          put(task_claim))
        .route("/api/tasks/:id/complete",       put(ok))
        .route("/api/tasks/:id/unclaim",        put(ok))
        .with_state(state)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn ok(_: State<S>) -> Json<Value> {
    Json(json!({"ok": true}))
}

async fn queue_items(State(st): State<S>) -> Json<Value> {
    let s = st.read().await;
    Json(json!({"items": s.queue_items}))
}

async fn item_claim(State(st): State<S>, Path(id): Path<String>) -> impl IntoResponse {
    let code = st.read().await.item_claim_status;
    let sc = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
    (sc, Json(json!({"ok": code == 200, "item": {"id": id}}))).into_response()
}

async fn task_list(
    State(st): State<S>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let s = st.read().await;
    let filter = params.get("status").cloned().unwrap_or_default();
    let matched: Vec<&Value> = if filter.is_empty() {
        s.tasks.iter().collect()
    } else {
        s.tasks.iter().filter(|t| t["status"].as_str() == Some(&filter)).collect()
    };
    let count = matched.len() as u64;
    Json(json!({"tasks": matched, "count": count}))
}

async fn task_claim(State(st): State<S>, Path(id): Path<String>) -> impl IntoResponse {
    let code = st.read().await.task_claim_status;
    let sc = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
    (sc, Json(json!({"ok": code == 200, "task": {"id": id, "title": "mock task", "status": "claimed"}}))).into_response()
}
