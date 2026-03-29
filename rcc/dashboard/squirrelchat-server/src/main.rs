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
