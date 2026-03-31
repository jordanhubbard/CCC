mod db;
mod models;
mod ws;
mod routes;

use std::sync::Arc;

use tower_http::cors::{CorsLayer, Any};
use tracing::info;

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub db: db::Db,
    pub hub: ws::Hub,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let db_path = std::env::var("SQUIRRELCHAT_DB").unwrap_or_else(|_| "squirrelchat.db".into());
    let db = db::Db::open(&db_path)?;
    db.migrate()?;

    let hub = ws::Hub::new();

    let state: SharedState = Arc::new(AppState { db, hub });

    // ── Scheduled message delivery worker ─────────────────────────────────────
    {
        let worker_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                let pending = match worker_state.db.get_pending_scheduled_messages(now) {
                    Ok(rows) => rows,
                    Err(e) => { tracing::warn!("scheduled_messages query failed: {}", e); continue; }
                };
                for (id, channel_id, sender, text, _deliver_at) in pending {
                    match worker_state.db.insert_message(&sender, &text, &channel_id, &[], None) {
                        Ok(msg_id) => {
                            if let Ok(Some(msg)) = worker_state.db.get_message(msg_id) {
                                worker_state.hub.broadcast(&crate::models::ServerFrame::Message {
                                    message: crate::models::MessageWire::from(msg),
                                });
                                worker_state.hub.broadcast(&crate::models::ServerFrame::UnreadUpdate {
                                    counts: std::collections::HashMap::new(),
                                });
                            }
                            let _ = worker_state.db.mark_scheduled_delivered(&id);
                            info!("Delivered scheduled message {} to #{}", id, channel_id);
                        }
                        Err(e) => { tracing::warn!("Failed to deliver scheduled message {}: {}", id, e); }
                    }
                }
            }
        });
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = routes::build_router(state).layer(cors);

    let port = std::env::var("SQUIRRELCHAT_PORT").unwrap_or_else(|_| "8793".into());
    let addr = format!("0.0.0.0:{}", port);
    info!("SquirrelChat v2 listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
