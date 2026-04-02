use axum::Router;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{CorsLayer, Any};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod routes;
pub mod state;
pub mod brain;
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
    let projects_path = std::env::var("PROJECTS_PATH")
        .unwrap_or_else(|_| format!("{}/projects.json", data_dir));

    let auth_tokens: std::collections::HashSet<String> = std::env::var("RCC_AUTH_TOKENS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Build optional process supervisor
    let supervisor_handle = if std::env::var("SUPERVISOR_ENABLED").unwrap_or_default() == "true" {
        let tokenhub_bin = std::env::var("TOKENHUB_BIN")
            .unwrap_or_else(|_| "/home/jkh/tokenhub/bin/tokenhub".to_string());
        let squirrelchat_bin = std::env::var("SQUIRRELCHAT_BIN").unwrap_or_else(|_| {
            std::process::Command::new("which")
                .arg("squirrelchat-server")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/usr/local/bin/squirrelchat-server".to_string())
        });
        let processes = vec![
            supervisor::ManagedProcess {
                name: "tokenhub".to_string(),
                command: tokenhub_bin,
                args: vec![],
                env: vec![],
                health_url: Some("http://127.0.0.1:8090/health".to_string()),
                restart_delay_ms: 5000,
            },
            supervisor::ManagedProcess {
                name: "squirrelchat".to_string(),
                command: squirrelchat_bin,
                args: vec![],
                env: vec![],
                health_url: Some("http://127.0.0.1:8793/health".to_string()),
                restart_delay_ms: 5000,
            },
        ];
        let (sup, handle) = supervisor::Supervisor::new(processes);
        tokio::spawn(sup.run());
        tracing::info!("Supervisor enabled: managing tokenhub + squirrelchat");
        Some(handle)
    } else {
        tracing::info!("Supervisor disabled (set SUPERVISOR_ENABLED=true to enable)");
        None
    };

    // Build MinIO/S3 client
    let s3_bucket = std::env::var("MINIO_BUCKET").unwrap_or_else(|_| "agents".to_string());
    let s3_client = {
        let endpoint = std::env::var("MINIO_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:9000".to_string());
        let access_key = std::env::var("MINIO_ACCESS_KEY").ok();
        let secret_key = std::env::var("MINIO_SECRET_KEY").ok();
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
                tracing::warn!("MINIO_ACCESS_KEY or MINIO_SECRET_KEY not set — S3/AgentFS disabled");
                None
            }
        }
    };

    let app_state = Arc::new(AppState {
        auth_tokens,
        queue_path,
        agents_path,
        secrets_path,
        bus_log_path,
        projects_path,
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
        .merge(routes::agentos::router())
        .merge(routes::memory::router())
        .merge(routes::issues::router())
        .merge(routes::fs::router())
        .merge(routes::supervisor::router())
        .merge(routes::conversations::router())
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
