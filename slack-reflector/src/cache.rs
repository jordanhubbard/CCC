use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::time::{Duration, interval};
use tracing::{info, error};

use crate::slack_api::SlackApi;

/// Bidirectional name<->ID cache for one workspace
#[derive(Clone)]
pub struct WorkspaceCache {
    pub workspace_name: String,
    /// username -> user_id
    pub users_by_name: Arc<DashMap<String, String>>,
    /// user_id -> username
    pub users_by_id: Arc<DashMap<String, String>>,
    /// channel_name -> channel_id
    pub channels_by_name: Arc<DashMap<String, String>>,
    /// channel_id -> channel_name
    pub channels_by_id: Arc<DashMap<String, String>>,
    /// This bot's own user ID (to filter self-messages)
    pub bot_user_id: String,
}

impl WorkspaceCache {
    pub async fn new(api: &SlackApi) -> Result<Self> {
        let bot_user_id = api.get_bot_user_id().await?;
        info!("[{}] bot user ID: {}", api.workspace_name, bot_user_id);

        let cache = Self {
            workspace_name: api.workspace_name.clone(),
            users_by_name: Arc::new(DashMap::new()),
            users_by_id: Arc::new(DashMap::new()),
            channels_by_name: Arc::new(DashMap::new()),
            channels_by_id: Arc::new(DashMap::new()),
            bot_user_id,
        };

        cache.refresh(api).await?;
        Ok(cache)
    }

    pub async fn refresh(&self, api: &SlackApi) -> Result<()> {
        // Refresh users
        let users = api.fetch_users().await?;
        self.users_by_name.clear();
        self.users_by_id.clear();
        for (name, id) in &users {
            self.users_by_name.insert(name.clone(), id.clone());
            self.users_by_id.insert(id.clone(), name.clone());
        }

        // Refresh channels
        let channels = api.fetch_channels().await?;
        self.channels_by_name.clear();
        self.channels_by_id.clear();
        for (name, id) in &channels {
            self.channels_by_name.insert(name.clone(), id.clone());
            self.channels_by_id.insert(id.clone(), name.clone());
        }

        info!(
            "[{}] cache refreshed: {} users, {} channels",
            self.workspace_name,
            self.users_by_name.len(),
            self.channels_by_name.len()
        );
        Ok(())
    }

    /// Resolve a user_id to a username
    pub fn user_name(&self, user_id: &str) -> Option<String> {
        self.users_by_id.get(user_id).map(|v| v.clone())
    }

    /// Resolve a channel_id to a channel name
    pub fn channel_name(&self, channel_id: &str) -> Option<String> {
        self.channels_by_id.get(channel_id).map(|v| v.clone())
    }

    /// Resolve a channel name to a channel_id
    pub fn channel_id(&self, name: &str) -> Option<String> {
        self.channels_by_name.get(name).map(|v| v.clone())
    }

    /// Check if a user_id belongs to the bot itself
    pub fn is_bot_message(&self, user_id: &str) -> bool {
        user_id == self.bot_user_id
    }
}

/// Spawn a background task that refreshes the cache periodically
pub fn spawn_cache_refresher(
    cache: WorkspaceCache,
    api: SlackApi,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(interval_secs));
        loop {
            tick.tick().await;
            if let Err(e) = cache.refresh(&api).await {
                error!(
                    "[{}] cache refresh failed: {:#}",
                    cache.workspace_name, e
                );
            }
        }
    });
}
