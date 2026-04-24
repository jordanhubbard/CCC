use acc_server::{brain, build_app, config, db, dispatch, routes, state, AppState};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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

    let supervisor_handle = if cfg.supervisor_enabled {
        let processes = vec![acc_server::supervisor::ManagedProcess {
            name: "tokenhub".to_string(),
            command: cfg.tokenhub_bin.clone(),
            args: vec![],
            env: vec![],
            health_url: Some("http://127.0.0.1:8090/health".to_string()),
            restart_delay_ms: 5000,
        }];
        let (sup, handle) = acc_server::supervisor::Supervisor::new(processes);
        tokio::spawn(sup.run());
        tracing::info!("Supervisor enabled: managing tokenhub");
        Some(handle)
    } else {
        tracing::info!("Supervisor disabled");
        None
    };

    tracing::info!("AccFS root: {}", cfg.fs_root);

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

    let fleet_db_path = std::env::var("ACC_DATA_DIR")
        .map(|d| format!("{}/acc.db", d))
        .or_else(|_| std::env::var("ACC_DB_PATH"))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home".to_string());
            format!("{}/.acc/data/acc.db", home)
        });
    let fleet_db = db::open_fleet(&fleet_db_path).expect("failed to open fleet database");
    let fleet_db = Arc::new(tokio::sync::Mutex::new(fleet_db));

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
        fleet_db: fleet_db.clone(),
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
        soul_store: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        blob_store: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        blobs_path: format!("{}/blobs", cfg.data_dir),
        dlq_path: format!("{}/bus-dlq.jsonl", cfg.data_dir),
        user_token_roles: std::sync::RwLock::new(std::collections::HashMap::new()),
        watchdog: routes::watchdog::WatchdogState::new(),
    });

    state::load_all(&app_state).await;
    routes::lessons::load_lessons().await;
    routes::metrics::load_metrics().await;
    routes::issues::load_issues().await;
    routes::conversations::load_conversations().await;

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
                drop(conn);
            }
            Err(e) => tracing::warn!("Failed to open SQLite database {}: {}", db_path, e),
        }
    }

    let app = build_app(app_state.clone());

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

    let flush_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            state::flush_queue(&flush_state).await;
        }
    });

    let brain_arc = app_state.brain.clone();
    let brain_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to build reqwest client");
    tokio::spawn(brain::run_brain_worker(brain_arc, brain_client));

    let scanner_state = app_state.clone();
    tokio::spawn(routes::projects::run_beads_scanner(scanner_state));

    tokio::spawn(dispatch::run(app_state.clone()));

    let watchdog_state = app_state.clone();
    tokio::spawn(routes::watchdog::run_watchdog(watchdog_state));

    {
        let db = fleet_db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(900));
            loop {
                interval.tick().await;
                let conn = db.lock().await;
                let now = chrono::Utc::now().to_rfc3339();
                let _ = conn.execute(
                    "UPDATE fleet_tasks SET status='open', claimed_by=NULL, claimed_at=NULL, claim_expires_at=NULL, updated_at=?1 WHERE status='claimed' AND claim_expires_at IS NOT NULL AND claim_expires_at < ?1",
                    rusqlite::params![now],
                );
            }
        });
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(app_state.clone()))
        .await
        .unwrap();

    tracing::info!("Flushing state before exit...");
    state::flush_queue(&app_state).await;
    tracing::info!("Shutdown complete.");
}

async fn shutdown_signal(state: Arc<AppState>) {
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
        _ = ctrl_c    => { tracing::info!("Received Ctrl+C, shutting down"); },
        _ = terminate => { tracing::info!("Received SIGTERM, shutting down"); },
    }

    state::flush_queue(&state).await;
}
