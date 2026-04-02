use axum::{
    Router,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{CorsLayer, Any};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod routes;
pub mod state;

pub use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "rcc_server=info,tower_http=info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let port: u16 = std::env::var("RCC_PORT")
        .unwrap_or_else(|_| "8789".to_string())
        .parse()
        .unwrap_or(8789);

    let data_dir = std::env::var("RCC_DATA_DIR")
        .unwrap_or_else(|_| "./data".to_string());

    let queue_path = std::env::var("QUEUE_PATH")
        .unwrap_or_else(|_| format!("{}/queue.json", data_dir));
    let agents_path = std::env::var("AGENTS_PATH")
        .unwrap_or_else(|_| format!("{}/agents.json", data_dir));
    let secrets_path = std::env::var("SECRETS_PATH")
        .unwrap_or_else(|_| format!("{}/secrets.json", data_dir));
    let bus_log_path = std::env::var("BUS_LOG_PATH")
        .unwrap_or_else(|_| format!("{}/bus.jsonl", data_dir));

    let auth_tokens: std::collections::HashSet<String> = std::env::var("RCC_AUTH_TOKENS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let app_state = Arc::new(AppState {
        auth_tokens,
        queue_path,
        agents_path,
        secrets_path,
        bus_log_path,
        queue: RwLock::new(state::QueueData::default()),
        agents: RwLock::new(serde_json::Value::Object(serde_json::Map::new())),
        secrets: RwLock::new(serde_json::Map::new()),
        bus_tx: tokio::sync::broadcast::channel(256).0,
        bus_seq: std::sync::atomic::AtomicU64::new(0),
        start_time: std::time::SystemTime::now(),
    });

    // Load persisted state
    state::load_all(&app_state).await;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .merge(routes::health::router())
        .merge(routes::queue::router())
        .merge(routes::agents::router())
        .merge(routes::secrets::router())
        .merge(routes::bus::router())
        .layer(cors)
        .with_state(app_state.clone());

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("Failed to bind port");

    tracing::info!("rcc-server listening on port {}", port);
    tracing::info!(
        "Auth: {} token(s) configured",
        if app_state.auth_tokens.is_empty() { "OPEN".to_string() } else { format!("{}", app_state.auth_tokens.len()) }
    );

    // Spawn periodic flush
    let flush_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            state::flush_queue(&flush_state).await;
        }
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => { tracing::info!("Received Ctrl+C, shutting down"); },
        _ = terminate => { tracing::info!("Received SIGTERM, shutting down"); },
    }
}
