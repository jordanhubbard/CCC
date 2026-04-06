mod cache;
mod config;
mod health;
mod reflector;
mod slack_api;
mod socket_mode;
mod thread_map;

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::info;

use cache::{WorkspaceCache, spawn_cache_refresher};
use config::Config;
use reflector::run_reflector;
use slack_api::SlackApi;
use socket_mode::run_socket_mode;
use thread_map::ThreadMap;

fn setup_logging(level: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load config
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.yaml"));

    let config = Config::load(&config_path)?;
    setup_logging(&config.log_level);

    info!("slack-reflector starting");
    info!(
        "workspaces: {} <-> {}",
        config.workspaces[0].name, config.workspaces[1].name
    );

    // Build exclusion sets
    let exclude_channels: HashSet<String> = config.exclude_channels.iter().cloned().collect();
    let exclude_users: HashSet<String> = config.exclude_users.iter().cloned().collect();

    info!(
        "excluding {} channels, {} users",
        exclude_channels.len(),
        exclude_users.len()
    );

    // Create API clients
    let apis = [
        SlackApi::new(&config.workspaces[0].name, &config.workspaces[0].bot_token),
        SlackApi::new(&config.workspaces[1].name, &config.workspaces[1].bot_token),
    ];

    // Build caches (fetches users + channels on startup)
    info!("building workspace caches...");
    let caches = [
        WorkspaceCache::new(&apis[0]).await?,
        WorkspaceCache::new(&apis[1]).await?,
    ];

    // Report matched pairs
    let mut matched_channels = 0;
    let mut matched_users = 0;
    for entry in caches[0].channels_by_name.iter() {
        let name = entry.key();
        if !exclude_channels.contains(name) && caches[1].channels_by_name.contains_key(name) {
            info!("channel pair: #{}", name);
            matched_channels += 1;
        }
    }
    for entry in caches[0].users_by_name.iter() {
        let name = entry.key();
        if !exclude_users.contains(name) && caches[1].users_by_name.contains_key(name) {
            matched_users += 1;
        }
    }
    info!(
        "active pairs: {} channels, {} users",
        matched_channels, matched_users
    );

    // Spawn cache refreshers
    spawn_cache_refresher(
        caches[0].clone(),
        apis[0].clone(),
        config.cache_refresh_interval_secs,
    );
    spawn_cache_refresher(
        caches[1].clone(),
        apis[1].clone(),
        config.cache_refresh_interval_secs,
    );

    // Create message channel
    let (tx, rx) = mpsc::channel(1024);

    // Spawn Socket Mode listeners for both workspaces
    let tx0 = tx.clone();
    let name0 = config.workspaces[0].name.clone();
    let app_token0 = config.workspaces[0].app_token.clone();
    tokio::spawn(async move {
        run_socket_mode(0, name0, app_token0, tx0).await;
    });

    let tx1 = tx.clone();
    let name1 = config.workspaces[1].name.clone();
    let app_token1 = config.workspaces[1].app_token.clone();
    tokio::spawn(async move {
        run_socket_mode(1, name1, app_token1, tx1).await;
    });

    // Drop the original tx so rx closes when both socket tasks end
    drop(tx);

    // Spawn health check server
    tokio::spawn(health::run_health_server(config.health_port));

    // Run the reflector (blocks forever)
    run_reflector(rx, apis, caches, exclude_channels, exclude_users, ThreadMap::new()).await;

    Ok(())
}
