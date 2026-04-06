use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{debug, warn, info};

#[allow(dead_code)]
/// Slack Web API client for a single workspace
#[derive(Clone)]
pub struct SlackApi {
    pub workspace_name: String,
    bot_token: String,
    client: Client,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct SlackResponse {
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsersListResponse {
    ok: bool,
    error: Option<String>,
    members: Option<Vec<SlackUser>>,
    response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
struct ConversationsListResponse {
    ok: bool,
    error: Option<String>,
    channels: Option<Vec<SlackChannel>>,
    response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
struct ResponseMetadata {
    next_cursor: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct SlackUser {
    id: String,
    name: String,         // the short username
    deleted: Option<bool>,
    is_bot: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct SlackChannel {
    id: String,
    name: String,
    is_archived: Option<bool>,
    is_member: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ChatPostResponse {
    ok: bool,
    error: Option<String>,
    ts: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AuthTestResponse {
    ok: bool,
    error: Option<String>,
    user_id: Option<String>,
    bot_id: Option<String>,
}

impl SlackApi {
    pub fn new(workspace_name: &str, bot_token: &str) -> Self {
        Self {
            workspace_name: workspace_name.to_string(),
            bot_token: bot_token.to_string(),
            client: Client::new(),
        }
    }

    /// Get the bot's own user ID so we can ignore our own messages
    pub async fn get_bot_user_id(&self) -> Result<String> {
        let resp: AuthTestResponse = self
            .client
            .post("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            bail!(
                "[{}] auth.test failed: {}",
                self.workspace_name,
                resp.error.unwrap_or_default()
            );
        }

        resp.user_id
            .context("auth.test returned no user_id")
    }

    /// Fetch all users: returns username -> user_id mapping
    pub async fn fetch_users(&self) -> Result<HashMap<String, String>> {
        let mut map = HashMap::new();
        let mut cursor = String::new();

        loop {
            let mut params = vec![("limit", "500".to_string())];
            if !cursor.is_empty() {
                params.push(("cursor", cursor.clone()));
            }

            let resp: UsersListResponse = self
                .client
                .get("https://slack.com/api/users.list")
                .bearer_auth(&self.bot_token)
                .query(&params)
                .send()
                .await?
                .json()
                .await?;

            if !resp.ok {
                bail!(
                    "[{}] users.list failed: {}",
                    self.workspace_name,
                    resp.error.unwrap_or_default()
                );
            }

            if let Some(members) = resp.members {
                for user in members {
                    if user.deleted.unwrap_or(false) {
                        continue;
                    }
                    debug!(
                        "[{}] user: {} -> {}",
                        self.workspace_name, user.name, user.id
                    );
                    map.insert(user.name, user.id);
                }
            }

            match resp.response_metadata.and_then(|m| m.next_cursor) {
                Some(c) if !c.is_empty() => cursor = c,
                _ => break,
            }
        }

        info!(
            "[{}] cached {} users",
            self.workspace_name,
            map.len()
        );
        Ok(map)
    }

    /// Fetch all channels the bot is in: returns channel_name -> channel_id mapping
    pub async fn fetch_channels(&self) -> Result<HashMap<String, String>> {
        let mut map = HashMap::new();
        let mut cursor = String::new();

        loop {
            let mut params = vec![
                ("limit", "500".to_string()),
                ("types", "public_channel,private_channel".to_string()),
            ];
            if !cursor.is_empty() {
                params.push(("cursor", cursor.clone()));
            }

            let resp: ConversationsListResponse = self
                .client
                .get("https://slack.com/api/conversations.list")
                .bearer_auth(&self.bot_token)
                .query(&params)
                .send()
                .await?
                .json()
                .await?;

            if !resp.ok {
                bail!(
                    "[{}] conversations.list failed: {}",
                    self.workspace_name,
                    resp.error.unwrap_or_default()
                );
            }

            if let Some(channels) = resp.channels {
                for ch in channels {
                    if ch.is_archived.unwrap_or(false) {
                        continue;
                    }
                    debug!(
                        "[{}] channel: #{} -> {}",
                        self.workspace_name, ch.name, ch.id
                    );
                    map.insert(ch.name, ch.id);
                }
            }

            match resp.response_metadata.and_then(|m| m.next_cursor) {
                Some(c) if !c.is_empty() => cursor = c,
                _ => break,
            }
        }

        info!(
            "[{}] cached {} channels",
            self.workspace_name,
            map.len()
        );
        Ok(map)
    }

    /// Post a message to a channel, optionally in a thread
    pub async fn post_message(
        &self,
        channel_id: &str,
        text: &str,
        username: &str,
        thread_ts: Option<&str>,
    ) -> Result<Option<String>> {
        let mut body = serde_json::json!({
            "channel": channel_id,
            "text": text,
            "username": username,
            "unfurl_links": false,
            "unfurl_media": false,
        });

        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let resp: ChatPostResponse = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            warn!(
                "[{}] chat.postMessage failed: {} (channel={}, user={})",
                self.workspace_name,
                resp.error.as_deref().unwrap_or("unknown"),
                channel_id,
                username
            );
            return Ok(None);
        }

        Ok(resp.ts)
    }
}
