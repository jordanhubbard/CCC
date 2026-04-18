use axum::Router;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub mod brain;
pub mod config;
pub mod db;
mod routes;
pub mod state;
pub mod supervisor;

pub use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "acc_server=info,tower_http=info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = config::load();
    let port = cfg.port;

    // Build optional process supervisor
    let supervisor_handle = if cfg.supervisor_enabled {
        let processes = vec![supervisor::ManagedProcess {
            name: "tokenhub".to_string(),
            command: cfg.tokenhub_bin.clone(),
            args: vec![],
            env: vec![],
            health_url: Some("http://127.0.0.1:8090/health".to_string()),
            restart_delay_ms: 5000,
        }];
        let (sup, handle) = supervisor::Supervisor::new(processes);
        tokio::spawn(sup.run());
        tracing::info!("Supervisor enabled: managing tokenhub");
        Some(handle)
    } else {
        tracing::info!("Supervisor disabled");
        None
    };

    tracing::info!("AccFS root: {}", cfg.fs_root);

    // Open auth DB (always-on)
    let auth_conn = match db::open_auth(&cfg.auth_db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to open auth DB at {}: {}", cfg.auth_db_path, e);
            std::process::exit(1);
        }
    };
    let initial_hashes: std::collections::HashSet<String> =
        db::auth_all_token_hashes(&auth_conn).into_iter().collect();
    tracing::info!("Auth DB: {} user(s) loaded", initial_hashes.len());
    let auth_db = Arc::new(tokio::sync::Mutex::new(auth_conn));

    // Clone paths before they're moved into AppState (needed for SQLite migration below)
    let queue_path_c    = cfg.queue_path.clone();
    let agents_path_c   = cfg.agents_path.clone();
    let secrets_path_c  = cfg.secrets_path.clone();
    let projects_path_c = cfg.projects_path.clone();
    let db_path         = cfg.db_path.clone();
    let fs_root         = cfg.fs_root.clone();

    let app_state = Arc::new(AppState {
        auth_tokens: cfg.auth_tokens,
        user_token_hashes: std::sync::RwLock::new(initial_hashes),
        auth_db,
        queue_path: cfg.queue_path,
        agents_path: cfg.agents_path,
        secrets_path: cfg.secrets_path,
        bus_log_path: cfg.bus_log_path,
        projects_path: cfg.projects_path,
        queue: RwLock::new(state::QueueData::default()),
        agents: RwLock::new(serde_json::Value::Object(serde_json::Map::new())),
        secrets: RwLock::new(serde_json::Map::new()),
        projects: tokio::sync::RwLock::new(Vec::new()),
        brain: Arc::new(brain::BrainQueue::new()),
        bus_tx: tokio::sync::broadcast::channel(256).0,
        bus_seq: std::sync::atomic::AtomicU64::new(0),
        start_time: std::time::SystemTime::now(),
        fs_root,
        supervisor: supervisor_handle,
    });

    // Load persisted state
    state::load_all(&app_state).await;
    routes::lessons::load_lessons().await;
    routes::metrics::load_metrics().await;
    routes::issues::load_issues().await;
    routes::conversations::load_conversations().await;

    // Optional SQLite mode: open database and migrate JSON data on first run.
    if let Some(db_path) = &db_path {
        match db::open(db_path) {
            Ok(conn) => {
                db::migrate_from_json(
                    &conn,
                    &queue_path_c,
                    &agents_path_c,
                    &secrets_path_c,
                    &projects_path_c,
                );
                tracing::info!("SQLite mode active: {}", db_path);
                // conn is not yet stored in AppState — see db.rs for next steps
                // when full SQLite runtime is enabled. For now, migration only.
                drop(conn);
            }
            Err(e) => tracing::warn!("Failed to open SQLite database {}: {}", db_path, e),
        }
    }

    // CORS — configurable via CCC_CORS_ORIGINS env var.
    // This server runs on Tailscale (internal network), so Any is the safe default.
    // For public-facing deployments, set CCC_CORS_ORIGINS=https://yourdomain.com
    let cors = match std::env::var("CCC_CORS_ORIGINS").ok().as_deref() {
        Some(origins) if !origins.is_empty() && origins != "*" => {
            let parsed: Vec<axum::http::HeaderValue> = origins
                .split(',')
                .filter_map(|o| o.trim().parse().ok())
                .collect();
            CorsLayer::new()
                .allow_origin(parsed)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::PUT,
                    axum::http::Method::PATCH,
                    axum::http::Method::DELETE,
                ])
                .allow_headers([
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::CONTENT_TYPE,
                ])
        }
        _ => CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any),
    };

    // Build router — API routes first, then static SPA fallback
    let mut app = Router::new()
        .merge(routes::health::router())
        .merge(routes::queue::router())
        .merge(routes::agents::router())
        .merge(routes::secrets::router())
        .merge(routes::bus::router())
        .merge(routes::projects::router())
        .merge(routes::brain::router())
        .merge(routes::services::router())
        .merge(routes::lessons::router())
        .merge(routes::exec::router())
        .merge(routes::geek::router())
        .merge(routes::ui::router())
        .merge(routes::memory::router())
        .merge(routes::issues::router())
        .merge(routes::fs::router())
        .merge(routes::supervisor::router())
        .merge(routes::conversations::router())
        .merge(routes::setup::router())
        .merge(routes::providers::router())
        .merge(routes::acp::router())
        .merge(routes::models::router())
        .merge(routes::auth::router());

    // Serve WASM SPA as fallback if DASHBOARD_DIST is set.
    // This allows acc-server to serve the dashboard directly (no dashboard-server needed).
    if let Ok(dist) = std::env::var("DASHBOARD_DIST") {
        if !dist.is_empty() && std::path::Path::new(&dist).exists() {
            use tower_http::services::{ServeDir, ServeFile};
            let index = format!("{}/index.html", dist);
            tracing::info!("Serving dashboard SPA from {}", dist);
            app = app.fallback_service(
                ServeDir::new(&dist).not_found_service(ServeFile::new(index)),
            );
        }
    }

    let app = app.layer(cors).with_state(app_state.clone());

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("Failed to bind port");

    tracing::info!("acc-server listening on port {}", port);
    tracing::info!(
        "Auth: {} token(s) configured",
        if app_state.auth_tokens.is_empty() {
            "OPEN (no tokens — all requests allowed)".to_string()
        } else {
            format!("{}", app_state.auth_tokens.len())
        }
    );

    // Spawn periodic flush (every 30s)
    let flush_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            state::flush_queue(&flush_state).await;
        }
    });

    // Spawn brain worker
    let brain_arc = app_state.brain.clone();
    let brain_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to build reqwest client");
    tokio::spawn(brain::run_brain_worker(brain_arc, brain_client));

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(app_state.clone()))
        .await
        .unwrap();

    // Final flush on clean exit
    tracing::info!("Flushing state before exit...");
    state::flush_queue(&app_state).await;
    tracing::info!("Shutdown complete.");
}

async fn shutdown_signal(state: Arc<AppState>) {
    use tokio::signal;
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
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
        _ = ctrl_c  => { tracing::info!("Received Ctrl+C, shutting down"); },
        _ = terminate => { tracing::info!("Received SIGTERM, shutting down"); },
    }

    // Flush state immediately on signal before axum drains connections
    state::flush_queue(&state).await;
}
