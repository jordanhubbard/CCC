use axum::{
    Router,
    routing::{get, put},
    extract::{State, Request},
    response::Response,
    http::{StatusCode, HeaderMap, Method},
    body::Body,
    middleware::{self, Next},
};
use std::sync::Arc;
use std::time::Instant;
use tower_http::services::ServeDir;
use tracing_subscriber::prelude::*;

#[derive(Clone)]
struct AppState {
    rcc_url: String,
    sc_url: String,
    agent_token: String,
    client: reqwest::Client,
    stream_client: reqwest::Client,
    start_time: Arc<Instant>,
}

fn json_error(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::from(format!(r#"{{"error":"{}"}}"#, msg)))
        .unwrap()
}

// POST paths exempt from Bearer auth (safe public/dev endpoints)
const AUTH_EXEMPT_POSTS: &[&str] = &[
    "/api/playground/run",
    "/api/brain/request",
];

// --- Auth middleware for mutating endpoints ---
// Reads Authorization: Bearer <token> from request headers.
// Allows GETs through. Blocks POST/PATCH/DELETE without valid token.
// Some POST paths are exempt (see AUTH_EXEMPT_POSTS).
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response<Body> {
    let method = req.method().clone();
    let is_mutating = matches!(method, Method::POST | Method::PATCH | Method::DELETE | Method::PUT);

    if is_mutating && !state.agent_token.is_empty() {
        let path = req.uri().path().to_string();
        let is_exempt = method == Method::POST
            && AUTH_EXEMPT_POSTS.iter().any(|p| path == *p);

        if !is_exempt {
            let auth_ok = req
                .headers()
                .get("Authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|tok| tok.trim() == state.agent_token)
                .unwrap_or(false);

            if !auth_ok {
                return json_error(StatusCode::UNAUTHORIZED, "valid Bearer token required for mutations");
            }
        }
    }

    next.run(req).await
}

async fn proxy_to_upstream(
    state: &AppState,
    base_url: &str,
    method: reqwest::Method,
    path: &str,
    headers: &HeaderMap,
    body: Option<bytes::Bytes>,
) -> Response<Body> {
    let url = format!("{}{}", base_url, path);

    let mut builder = state
        .client
        .request(method, &url)
        .header("Authorization", format!("Bearer {}", state.agent_token));

    // Forward content-type if present
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        builder = builder.header("Content-Type", ct);
    }

    if let Some(b) = body {
        builder = builder.body(b);
    }

    match builder.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let ct = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_string();
            match resp.bytes().await {
                Ok(bytes) => Response::builder()
                    .status(status)
                    .header("Content-Type", ct)
                    .header("Access-Control-Allow-Origin", "*")
                    .body(Body::from(bytes))
                    .unwrap(),
                Err(_) => json_error(StatusCode::BAD_GATEWAY, "upstream read error"),
            }
        }
        Err(e) => {
            tracing::warn!("Proxy {} error: {}", path, e);
            json_error(StatusCode::BAD_GATEWAY, &e.to_string())
        }
    }
}

async fn proxy_to_rcc(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    headers: &HeaderMap,
    body: Option<bytes::Bytes>,
) -> Response<Body> {
    proxy_to_upstream(state, &state.rcc_url.clone(), method, path, headers, body).await
}

async fn proxy_to_sc(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    headers: &HeaderMap,
    body: Option<bytes::Bytes>,
) -> Response<Body> {
    proxy_to_upstream(state, &state.sc_url.clone(), method, path, headers, body).await
}

// --- /api/* handlers ---
async fn api_get(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::GET, path, &headers, None).await
}

async fn api_post(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::POST, path, &headers, Some(body)).await
}

async fn api_patch(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::PATCH, path, &headers, Some(body)).await
}

async fn api_delete(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::DELETE, path, &headers, None).await
}

// --- /bus/* handlers ---
async fn bus_get(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::GET, path, &headers, None).await
}

async fn bus_post(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::POST, path, &headers, Some(body)).await
}

async fn bus_stream(State(state): State<Arc<AppState>>) -> Response<Body> {
    let url = format!("{}/bus/stream", state.rcc_url);
    match state
        .stream_client
        .get(&url)
        .header("Authorization", format!("Bearer {}", state.agent_token))
        .header("Accept", "text/event-stream")
        .send()
        .await
    {
        Ok(resp) => {
            let stream = resp.bytes_stream();
            Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .header("Connection", "keep-alive")
                .header("Access-Control-Allow-Origin", "*")
                .body(Body::from_stream(stream))
                .unwrap()
        }
        Err(e) => {
            tracing::warn!("SSE upstream error: {}", e);
            Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .body(Body::from(": upstream unavailable\n\n"))
                .unwrap()
        }
    }
}

// --- /sc/api/stream SSE passthrough (no timeout) ---
async fn sc_stream(State(state): State<Arc<AppState>>) -> Response<Body> {
    let url = format!("{}/api/stream", state.sc_url);
    match state
        .stream_client
        .get(&url)
        .header("Authorization", &std::env::var("SQUIRRELCHAT_ADMIN_TOKEN").unwrap_or_else(|_| "<YOUR_SC_TOKEN>".to_string()))
        .header("Accept", "text/event-stream")
        .send()
        .await
    {
        Ok(resp) => {
            let stream = resp.bytes_stream();
            Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .header("Connection", "keep-alive")
                .header("Access-Control-Allow-Origin", "*")
                .body(Body::from_stream(stream))
                .unwrap()
        }
        Err(e) => {
            tracing::warn!("SC SSE upstream error: {}", e);
            Response::builder()
                .status(200)
                .header("Content-Type", "text/event-stream")
                .header("Cache-Control", "no-cache")
                .body(Body::from(": sc upstream unavailable\n\n"))
                .unwrap()
        }
    }
}

// --- /sc/* handlers (SquirrelChat proxy) ---
async fn sc_get(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    // Strip /sc prefix, forward rest to squirrelchat
    let sc_path = path.strip_prefix("/sc").unwrap_or(path);
    let sc_path = if sc_path.is_empty() { "/" } else { sc_path };
    proxy_to_sc(&state, reqwest::Method::GET, sc_path, &headers, None).await
}

async fn sc_post(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    let sc_path = path.strip_prefix("/sc").unwrap_or(path);
    let sc_path = if sc_path.is_empty() { "/" } else { sc_path };
    proxy_to_sc(&state, reqwest::Method::POST, sc_path, &headers, Some(body)).await
}

async fn sc_patch(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    let sc_path = path.strip_prefix("/sc").unwrap_or(path);
    let sc_path = if sc_path.is_empty() { "/" } else { sc_path };
    proxy_to_sc(&state, reqwest::Method::PATCH, sc_path, &headers, Some(body)).await
}

async fn sc_delete(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    let sc_path = path.strip_prefix("/sc").unwrap_or(path);
    let sc_path = if sc_path.is_empty() { "/" } else { sc_path };
    proxy_to_sc(&state, reqwest::Method::DELETE, sc_path, &headers, None).await
}

// --- /s3/* handlers (MinIO S3 proxy via RCC) ---
async fn s3_get(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::GET, path, &headers, None).await
}

async fn s3_put(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::PUT, path, &headers, Some(body)).await
}

async fn s3_delete(
    State(state): State<Arc<AppState>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let path = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    proxy_to_rcc(&state, reqwest::Method::DELETE, path, &headers, None).await
}

// --- /health ---
async fn health(State(state): State<Arc<AppState>>) -> Response<Body> {
    let uptime_secs = state.start_time.elapsed().as_secs();

    // Check RCC connectivity (non-blocking, short timeout)
    let rcc_ok = state
        .client
        .get(format!("{}/health", state.rcc_url))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    // Check squirrelchat connectivity
    let sc_ok = state
        .client
        .get(format!("{}/health", state.sc_url))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    let body = format!(
        r#"{{"ok":true,"uptime_seconds":{},"rcc_ok":{},"version":"2.0.0"}}"#,
        uptime_secs, rcc_ok
    );
    let _ = sc_ok; // checked but not exposed in the spec response

    Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::from(body))
        .unwrap()
}

/// Build the Axum router with the given state (extracted for testability).
async fn activity_page() -> Response<Body> {
    Response::builder()
        .status(200)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(Body::from(include_str!("static/activity.html")))
        .unwrap()
}

/// SPA index.html route — returns index.html with 200 for any deep-linked path.
/// Used as the fallback route so /kanban, /agents, etc. all bootstrap the WASM app.
async fn spa_index(State(state): State<Arc<AppState>>) -> Response<Body> {
    let index_path = {
        let dist = std::env::var("DASHBOARD_DIST").unwrap_or_else(|_| "dist".to_string());
        format!("{}/index.html", dist)
    };
    match tokio::fs::read(&index_path).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(Body::from(bytes))
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("index.html not found"))
            .unwrap(),
    }
}

pub fn build_app(state: Arc<AppState>, dist: &str) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/activity", get(activity_page))
        .route("/bus/stream", get(bus_stream))
        .route("/api/*path", get(api_get).post(api_post).patch(api_patch).delete(api_delete))
        .route("/bus/*path", get(bus_get).post(bus_post))
        .route("/s3/*path", get(s3_get).put(s3_put).delete(s3_delete))
        .route("/sc/*path", get(sc_get).post(sc_post).patch(sc_patch).delete(sc_delete))
        // SPA deep-link routes — serve index.html for all named tab paths
        .route("/geek",      get(spa_index))
        .route("/kanban",    get(spa_index))
        .route("/chat",      get(spa_index))
        .route("/agents",    get(spa_index))
        .route("/issues",    get(spa_index))
        .route("/providers", get(spa_index))
        .route("/coding",    get(spa_index))
        .route("/services",  get(spa_index))
        .route("/timeline",  get(spa_index))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
        .fallback_service(
            ServeDir::new(dist).append_index_html_on_directories(true)
        )
}

/// Build AppState from environment / explicit values (used in tests and main).
pub fn build_state(rcc_url: String, sc_url: String, agent_token: String) -> Arc<AppState> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("HTTP client build failed");
    let stream_client = reqwest::Client::builder()
        .build()
        .expect("Stream client build failed");
    Arc::new(AppState {
        rcc_url,
        sc_url,
        agent_token,
        client,
        stream_client,
        start_time: Arc::new(Instant::now()),
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dashboard_server=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let port = std::env::var("RCC_DASHBOARD_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8790);

    let rcc_url = std::env::var("RCC_URL")
        .unwrap_or_else(|_| "http://localhost:8789".to_string());
    let sc_url = std::env::var("SC_URL")
        .unwrap_or_else(|_| "http://localhost:8793".to_string());
    let agent_token = std::env::var("RCC_AGENT_TOKEN").unwrap_or_default();
    let operator = std::env::var("OPERATOR_HANDLE").unwrap_or_else(|_| "jkh".to_string());
    let dist = std::env::var("DASHBOARD_DIST").unwrap_or_else(|_| "dist".to_string());

    let state = build_state(rcc_url.clone(), sc_url.clone(), agent_token);
    let app = build_app(state, &dist);

    tracing::info!(port, rcc_url, sc_url, operator, "RCC Dashboard v2 starting");
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("Failed to bind");
    axum::serve(listener, app).await.expect("Server error");
}

// ── Tests ──────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;
    use httpmock::prelude::*;
    use serde_json::{json, Value};

    /// Spin up a mock RCC + SC upstream and a TestServer wrapping our app.
    fn make_server(mock_rcc: &MockServer, mock_sc: &MockServer, token: &str) -> TestServer {
        let state = build_state(
            mock_rcc.base_url(),
            mock_sc.base_url(),
            token.to_string(),
        );
        let app = build_app(state, "/tmp"); // dist path irrelevant for API tests
        TestServer::new(app).expect("TestServer build failed")
    }

    // ── /health ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_returns_ok() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "");
        let resp = srv.get("/health").await;
        resp.assert_status_ok();
        let body: Value = resp.json();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn health_contains_uptime() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        // health endpoint probes upstream /health — mock them
        rcc.mock(|when, then| { when.method(GET).path("/health"); then.status(200).body("{}"); });
        sc.mock(|when, then| { when.method(GET).path("/health"); then.status(200).body("{}"); });
        let srv = make_server(&rcc, &sc, "");
        let body: Value = srv.get("/health").await.json();
        // field is uptime_seconds (not uptime_secs)
        assert!(body["uptime_seconds"].is_number(), "uptime_seconds must be present");
    }

    // ── /api/* proxy ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn api_get_proxies_to_rcc() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        // Proxy forwards full path including /api/ prefix
        rcc.mock(|when, then| {
            when.method(GET).path("/api/queue");
            then.status(200).json_body(json!({"items": [], "ok": true}));
        });
        let srv = make_server(&rcc, &sc, "");
        let body: Value = srv.get("/api/queue").await.json();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn api_post_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "secret");
        let resp = srv.post("/api/queue").await;
        resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_post_with_token_proxies_to_rcc() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        rcc.mock(|when, then| {
            when.method(POST).path("/api/queue");
            then.status(201).json_body(json!({"id": "test-item"}));
        });
        let srv = make_server(&rcc, &sc, "secret");
        let body: Value = srv
            .post("/api/queue")
            .add_header("Authorization", "Bearer secret")
            .json(&json!({"title": "test"}))
            .await
            .json();
        assert_eq!(body["id"], "test-item");
    }

    #[tokio::test]
    async fn api_patch_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "secret");
        let resp = srv.patch("/api/item/foo").await;
        resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_delete_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "secret");
        let resp = srv.delete("/api/item/foo").await;
        resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_get_propagates_rcc_error_status() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        rcc.mock(|when, then| {
            when.method(GET).path("/nonexistent");
            then.status(404).json_body(json!({"error": "not found"}));
        });
        let srv = make_server(&rcc, &sc, "");
        let resp = srv.get("/api/nonexistent").await;
        resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    }

    // ── /bus/* proxy ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn bus_get_proxies_to_rcc() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        // Proxy forwards full path including /bus/ prefix
        rcc.mock(|when, then| {
            when.method(GET).path("/bus/messages");
            then.status(200).json_body(json!({"messages": []}));
        });
        let srv = make_server(&rcc, &sc, "");
        let body: Value = srv.get("/bus/messages").await.json();
        assert_eq!(body["messages"], json!([]));
    }

    #[tokio::test]
    async fn bus_post_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "secret");
        let resp = srv.post("/bus/send").await;
        resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bus_post_with_token_proxies_to_rcc() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        rcc.mock(|when, then| {
            when.method(POST).path_contains("/bus/send");
            then.status(200).json_body(json!({"ok": true}));
        });
        let srv = make_server(&rcc, &sc, "tok");
        let body: Value = srv
            .post("/bus/send")
            .add_header("Authorization", "Bearer tok")
            .json(&json!({"to": "rocky", "message": "hi"}))
            .await
            .json();
        assert_eq!(body["ok"], true);
    }

    // ── /sc/* squirrelchat proxy ───────────────────────────────────────────────

    #[tokio::test]
    async fn sc_get_proxies_to_sc_upstream() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        // sc handlers strip the /sc prefix before forwarding
        sc.mock(|when, then| {
            when.method(GET).path("/channels");
            then.status(200).json_body(json!({"channels": ["#general"]}));
        });
        let srv = make_server(&rcc, &sc, "");
        let body: Value = srv.get("/sc/channels").await.json();
        assert_eq!(body["channels"][0], "#general");
    }

    #[tokio::test]
    async fn sc_post_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "secret");
        let resp = srv.post("/sc/messages").await;
        resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sc_post_with_token_proxies_to_sc() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        // sc handlers strip /sc prefix
        sc.mock(|when, then| {
            when.method(POST).path("/messages");
            then.status(201).json_body(json!({"id": "msg-1"}));
        });
        let srv = make_server(&rcc, &sc, "tok");
        let body: Value = srv
            .post("/sc/messages")
            .add_header("Authorization", "Bearer tok")
            .json(&json!({"text": "hello"}))
            .await
            .json();
        assert_eq!(body["id"], "msg-1");
    }

    #[tokio::test]
    async fn sc_patch_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "tok");
        srv.patch("/sc/messages/1").await
            .assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sc_delete_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "tok");
        srv.delete("/sc/messages/1").await
            .assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    // ── /s3/* MinIO S3 proxy ─────────────────────────────────────────────────

    #[tokio::test]
    async fn s3_get_proxies_to_rcc() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        rcc.mock(|when, then| {
            when.method(GET).path("/s3/agents");
            then.status(200).json_body(json!({"bucket": "agents", "objects": []}));
        });
        let srv = make_server(&rcc, &sc, "");
        let body: Value = srv.get("/s3/agents").await.json();
        assert_eq!(body["bucket"], "agents");
    }

    #[tokio::test]
    async fn s3_put_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "secret");
        let resp = srv.put("/s3/agents/test.json").await;
        resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn s3_put_with_token_proxies_to_rcc() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        rcc.mock(|when, then| {
            when.method(PUT).path("/s3/agents/test.json");
            then.status(200).json_body(json!({"ok": true, "bucket": "agents", "key": "test.json"}));
        });
        let srv = make_server(&rcc, &sc, "tok");
        let body: Value = srv
            .put("/s3/agents/test.json")
            .add_header("Authorization", "Bearer tok")
            .json(&json!({"data": "test"}))
            .await
            .json();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn s3_delete_without_token_returns_401() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "tok");
        srv.delete("/s3/agents/test.json").await
            .assert_status(axum::http::StatusCode::UNAUTHORIZED);
    }

    // ── Auth: no token configured = all mutations allowed ─────────────────────

    #[tokio::test]
    async fn mutations_allowed_when_no_token_configured() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        rcc.mock(|when, then| {
            when.method(POST).path("/api/queue");
            then.status(201).json_body(json!({"id": "x"}));
        });
        // Empty token = auth disabled
        let srv = make_server(&rcc, &sc, "");
        let resp = srv.post("/api/queue").json(&json!({})).await;
        resp.assert_status(axum::http::StatusCode::CREATED);
    }

    // ── CORS headers ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_has_cors_header() {
        let rcc = MockServer::start();
        let sc  = MockServer::start();
        let srv = make_server(&rcc, &sc, "");
        let resp = srv.get("/health").await;
        let cors = resp.header("access-control-allow-origin");
        assert_eq!(cors, "*");
    }
}
