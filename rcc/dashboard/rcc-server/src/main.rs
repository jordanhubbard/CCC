use axum::Router;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub mod brain;
pub mod config;
mod routes;
pub mod state;
pub mod supervisor;

pub use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "rcc_server=info,tower_http=info".into()),
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

    // Build MinIO/S3 client
    let s3_bucket = cfg.minio_bucket.clone();
    let s3_client = {
        let endpoint = cfg.minio_endpoint.clone();
        let access_key = cfg.minio_access_key.clone();
        let secret_key = cfg.minio_secret_key.clone();
        match (access_key, secret_key) {
            (Some(ak), Some(sk)) => {
                use aws_credential_types::Credentials;
                use aws_sdk_s3::config::Region;
                let creds = Credentials::new(ak, sk, None, None, "env");
                let s3_config = aws_sdk_s3::Config::builder()
                    .credentials_provider(creds)
                    .region(Region::new("us-east-1"))
                    .endpoint_url(endpoint)
                    .force_path_style(true)
                    .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                    .build();
                tracing::info!("S3/MinIO client initialized (bucket={})", s3_bucket);
                Some(Arc::new(aws_sdk_s3::Client::from_conf(s3_config)))
            }
            _ => {
                tracing::warn!("MINIO_ACCESS_KEY or MINIO_SECRET_KEY not set — S3/ClawFS disabled");
                None
            }
        }
    };

    let app_state = Arc::new(AppState {
        auth_tokens: cfg.auth_tokens,
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
        s3_client,
        s3_bucket,
        supervisor: supervisor_handle,
    });

    // Load persisted state
    state::load_all(&app_state).await;
    routes::lessons::load_lessons().await;
    routes::metrics::load_metrics().await;
    routes::issues::load_issues().await;
    routes::conversations::load_conversations().await;

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
        .layer(cors)
        .with_state(app_state.clone());

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("Failed to bind port");

    tracing::info!("rcc-server listening on port {}", port);
    tracing::info!(
        "Auth: {} token(s) configured",
        if app_state.auth_tokens.is_empty() {
            "OPEN".to_string()
        } else {
            format!("{}", app_state.auth_tokens.len())
        }
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

    // Spawn brain worker
    let brain_arc = app_state.brain.clone();
    let brain_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to build reqwest client");
    tokio::spawn(brain::run_brain_worker(brain_arc, brain_client));

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
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
        _ = ctrl_c => { tracing::info!("Received Ctrl+C, shutting down"); },
        _ = terminate => { tracing::info!("Received SIGTERM, shutting down"); },
    }
}
