use axum::{
    Router,
    routing::get,
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

// --- Auth middleware for mutating endpoints ---
// Reads Authorization: Bearer <token> from request headers.
// Allows GETs through. Blocks POST/PATCH/DELETE without valid token.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response<Body> {
    let method = req.method().clone();
    let is_mutating = matches!(method, Method::POST | Method::PATCH | Method::DELETE | Method::PUT);

    if is_mutating && !state.agent_token.is_empty() {
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
        r#"{{"ok":true,"uptime_seconds":{},"rcc_connected":{},"sc_connected":{},"version":"2.0.0"}}"#,
        uptime_secs, rcc_ok, sc_ok
    );

    Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::from(body))
        .unwrap()
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
        .unwrap_or_else(|_| "http://localhost:8790".to_string());

    let agent_token = std::env::var("RCC_AGENT_TOKEN").unwrap_or_default();

    let operator = std::env::var("OPERATOR_HANDLE").unwrap_or_else(|_| "jkh".to_string());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("HTTP client build failed");

    let stream_client = reqwest::Client::builder()
        .build()
        .expect("Stream client build failed");

    let start_time = Arc::new(Instant::now());

    let state = Arc::new(AppState {
        rcc_url: rcc_url.clone(),
        sc_url: sc_url.clone(),
        agent_token,
        client,
        stream_client,
        start_time,
    });

    let dist = std::env::var("DASHBOARD_DIST").unwrap_or_else(|_| "dist".to_string());

    // Route priority (axum picks most-specific first):
    //   /health      →  health check (no auth)
    //   /bus/stream  →  SSE passthrough (no timeout)
    //   /api/*       →  JSON proxy to RCC (GET open, POST/PATCH/DELETE require auth)
    //   /bus/*       →  bus proxy to RCC (GET open, POST requires auth)
    //   /sc/*        →  squirrelchat proxy (GET open, mutations require auth)
    //   fallback     →  serve dist/ static files (SPA)
    let app = Router::new()
        .route("/health", get(health))
        .route("/bus/stream", get(bus_stream))
        .route("/api/*path", get(api_get).post(api_post).patch(api_patch).delete(api_delete))
        .route("/bus/*path", get(bus_get).post(bus_post))
        .route("/sc/*path", get(sc_get).post(sc_post).patch(sc_patch).delete(sc_delete))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
        .fallback_service(ServeDir::new(&dist).append_index_html_on_directories(true));

    tracing::info!(
        port,
        rcc_url,
        sc_url,
        operator,
        "RCC Dashboard v2 starting"
    );
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("Failed to bind");
    axum::serve(listener, app).await.expect("Server error");
}
