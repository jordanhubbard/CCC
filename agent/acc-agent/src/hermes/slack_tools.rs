//! LLM-callable tools that expose Slack workspace introspection.
//!
//! Each tool wraps one Slack Web API method and is registered with the
//! `ToolRegistry` only when the gateway has resolved Slack tokens (i.e.,
//! when there is a workspace to talk to). The bot token is captured in
//! a shared `SlackApiClient` so multiple tools share one HTTP client.
//!
//! These tools read the workspace; they do not send messages — message
//! send goes through the gateway's normal post-handling path so audit
//! and threading semantics stay consistent.

use super::slack_api::SlackApiClient;
use super::tool::{Tool, ToolResult};
use acc_qdrant::{EmbedClient, QdrantClient};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

fn ok_json(v: Value) -> ToolResult {
    serde_json::to_string(&v).map_err(|e| format!("serialize: {e}"))
}

// ── users.info ────────────────────────────────────────────────────────────────

pub struct SlackUsersInfoTool {
    client: Arc<SlackApiClient>,
}

impl SlackUsersInfoTool {
    pub fn new(client: Arc<SlackApiClient>) -> Self {
        Self { client }
    }
}

impl Tool for SlackUsersInfoTool {
    fn name(&self) -> &str {
        "slack_users_info"
    }
    fn description(&self) -> &str {
        "Look up a Slack user by their user ID (e.g., U02ABCDEF). Returns the \
         full profile: real_name, display_name, email when available, time \
         zone, and avatar URL. Use this whenever you need to identify, \
         address, or learn about a person in the workspace."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "user_id": {
                    "type": "string",
                    "description": "Slack user ID, e.g. U02ABCDEF. Strip any leading @ or <@...>"
                }
            },
            "required": ["user_id"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let raw = input["user_id"].as_str().unwrap_or("").trim();
            let user_id = raw
                .trim_start_matches("<@")
                .trim_end_matches('>')
                .trim_start_matches('@');
            if user_id.is_empty() {
                return Err("user_id is required".to_string());
            }
            ok_json(self.client.users_info(user_id).await?)
        })
    }
}

// ── users.list ────────────────────────────────────────────────────────────────

pub struct SlackUsersListTool {
    client: Arc<SlackApiClient>,
}

impl SlackUsersListTool {
    pub fn new(client: Arc<SlackApiClient>) -> Self {
        Self { client }
    }
}

impl Tool for SlackUsersListTool {
    fn name(&self) -> &str {
        "slack_users_list"
    }
    fn description(&self) -> &str {
        "List users in the Slack workspace, paginated. Use limit (max 200) \
         to bound the page. The response includes a response_metadata.next_cursor \
         that you can pass back as cursor to fetch the next page."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "Page size (1-200, default 50)"},
                "cursor": {"type": "string", "description": "Cursor from a prior page (optional)"}
            }
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let limit = input["limit"]
                .as_u64()
                .map(|v| v.clamp(1, 200) as u32)
                .or(Some(50));
            let cursor = input["cursor"].as_str();
            ok_json(self.client.users_list(limit, cursor).await?)
        })
    }
}

// ── conversations.members ─────────────────────────────────────────────────────

pub struct SlackConversationsMembersTool {
    client: Arc<SlackApiClient>,
}

impl SlackConversationsMembersTool {
    pub fn new(client: Arc<SlackApiClient>) -> Self {
        Self { client }
    }
}

impl Tool for SlackConversationsMembersTool {
    fn name(&self) -> &str {
        "slack_conversations_members"
    }
    fn description(&self) -> &str {
        "List the user IDs that are members of a Slack channel. Pass the channel \
         ID (e.g., C0AMNRSN9EZ); resolve user IDs separately with slack_users_info \
         if you need names. Paginated via cursor."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {"type": "string", "description": "Channel ID (C…)"},
                "limit": {"type": "integer", "description": "Page size (1-1000, default 100)"},
                "cursor": {"type": "string", "description": "Cursor from a prior page (optional)"}
            },
            "required": ["channel"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let channel = input["channel"].as_str().unwrap_or("").trim();
            if channel.is_empty() {
                return Err("channel is required".to_string());
            }
            let limit = input["limit"]
                .as_u64()
                .map(|v| v.clamp(1, 1000) as u32)
                .or(Some(100));
            let cursor = input["cursor"].as_str();
            ok_json(
                self.client
                    .conversations_members(channel, limit, cursor)
                    .await?,
            )
        })
    }
}

// ── conversations.history ─────────────────────────────────────────────────────

pub struct SlackConversationsHistoryTool {
    client: Arc<SlackApiClient>,
}

impl SlackConversationsHistoryTool {
    pub fn new(client: Arc<SlackApiClient>) -> Self {
        Self { client }
    }
}

impl Tool for SlackConversationsHistoryTool {
    fn name(&self) -> &str {
        "slack_conversations_history"
    }
    fn description(&self) -> &str {
        "Read recent messages from a Slack channel. Pass the channel ID (C…). \
         The response is paginated via cursor and bounded by limit (max 100). \
         Optional oldest is a Slack timestamp (e.g., '1700000000.000000') that \
         restricts results to messages newer than that ts."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {"type": "string", "description": "Channel ID (C…)"},
                "limit": {"type": "integer", "description": "Page size (1-100, default 20)"},
                "cursor": {"type": "string", "description": "Cursor from a prior page (optional)"},
                "oldest": {"type": "string", "description": "Slack timestamp lower bound (optional)"}
            },
            "required": ["channel"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let channel = input["channel"].as_str().unwrap_or("").trim();
            if channel.is_empty() {
                return Err("channel is required".to_string());
            }
            let limit = input["limit"]
                .as_u64()
                .map(|v| v.clamp(1, 100) as u32)
                .or(Some(20));
            let cursor = input["cursor"].as_str();
            let oldest = input["oldest"].as_str();
            ok_json(
                self.client
                    .conversations_history(channel, limit, cursor, oldest)
                    .await?,
            )
        })
    }
}

// ── team.info ─────────────────────────────────────────────────────────────────

pub struct SlackTeamInfoTool {
    client: Arc<SlackApiClient>,
}

impl SlackTeamInfoTool {
    pub fn new(client: Arc<SlackApiClient>) -> Self {
        Self { client }
    }
}

impl Tool for SlackTeamInfoTool {
    fn name(&self) -> &str {
        "slack_team_info"
    }
    fn description(&self) -> &str {
        "Return metadata about the current Slack workspace: id, name, domain, \
         and icon. Use to confirm which workspace you are operating in."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }
    fn execute<'a>(
        &'a self,
        _input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move { ok_json(self.client.team_info().await?) })
    }
}

/// Construct the full set of Slack tools sharing one HTTP client.
pub fn all_slack_tools(client: Arc<SlackApiClient>) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(SlackUsersInfoTool::new(client.clone())),
        Box::new(SlackUsersListTool::new(client.clone())),
        Box::new(SlackConversationsMembersTool::new(client.clone())),
        Box::new(SlackConversationsHistoryTool::new(client.clone())),
        Box::new(SlackTeamInfoTool::new(client)),
    ]
}

// ── slack_memory_search ───────────────────────────────────────────────────────
//
// Backs onto the same Qdrant collection that `acc-agent slack-ingest` writes
// to. Embedding the user's query and searching the collection lets a bot
// recall context from any channel it has been a member of, across both
// workspaces, without re-paginating Slack history live. Registered only
// when the gateway has Qdrant + embed config available.

pub struct SlackMemorySearchTool {
    qdrant: Arc<QdrantClient>,
    embed: Arc<EmbedClient>,
    collection: String,
}

impl SlackMemorySearchTool {
    pub fn new(qdrant: Arc<QdrantClient>, embed: Arc<EmbedClient>, collection: String) -> Self {
        Self {
            qdrant,
            embed,
            collection,
        }
    }
}

impl Tool for SlackMemorySearchTool {
    fn name(&self) -> &str {
        "slack_memory_search"
    }
    fn description(&self) -> &str {
        "Search the long-term semantic memory of Slack messages the bot has \
         seen across every channel it is a member of, in either workspace. \
         Returns the top-N most relevant messages with their channel, user, \
         timestamp, and text. Use this to recall what was previously \
         discussed, find context for an ongoing topic, or identify who said \
         what about something. Optional `workspace` (`omgjkh` or `offtera`) \
         restricts to one workspace; `channel` restricts to one channel by \
         its short name (e.g., `random`)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query":     {"type": "string",  "description": "Natural-language query"},
                "workspace": {"type": "string",  "description": "Optional workspace filter (omgjkh|offtera)"},
                "channel":   {"type": "string",  "description": "Optional channel-name filter"},
                "limit":     {"type": "integer", "description": "Max results (1-50, default 10)"}
            },
            "required": ["query"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let query = input["query"].as_str().unwrap_or("").trim();
            if query.is_empty() {
                return Err("query is required".to_string());
            }
            let limit = input["limit"]
                .as_u64()
                .map(|v| v.clamp(1, 50))
                .unwrap_or(10);

            // Embed the query.
            let vectors = self
                .embed
                .embed(&[query])
                .await
                .map_err(|e| format!("embed: {e}"))?;
            let qvec = vectors
                .into_iter()
                .next()
                .ok_or_else(|| "embed returned no vector".to_string())?;

            // Optional workspace + channel filters layered as Qdrant
            // `must` clauses; both omitted means an unfiltered search.
            let mut filters: Vec<Value> = Vec::new();
            if let Some(ws) = input["workspace"].as_str().filter(|s| !s.is_empty()) {
                filters.push(json!({"key": "workspace", "match": {"value": ws}}));
            }
            if let Some(ch) = input["channel"].as_str().filter(|s| !s.is_empty()) {
                filters.push(json!({"key": "channel", "match": {"value": ch}}));
            }
            let filter = if filters.is_empty() {
                None
            } else {
                Some(json!({"must": filters}))
            };

            let hits = self
                .qdrant
                .search_points(&self.collection, &qvec, limit, filter)
                .await
                .map_err(|e| format!("qdrant search: {e}"))?;

            // Trim payloads down to the fields a model will actually
            // condition on; raw embeddings and 8 KB rich-text blocks are
            // not useful in a tool result.
            let formatted: Vec<Value> = hits
                .iter()
                .map(|h| {
                    let p = &h.payload;
                    json!({
                        "score":     h.score,
                        "workspace": p.get("workspace"),
                        "channel":   p.get("channel"),
                        "user":      p.get("user"),
                        "date":      p.get("date"),
                        "text":      p.get("text"),
                    })
                })
                .collect();

            serde_json::to_string_pretty(&json!({
                "collection": &self.collection,
                "hit_count":  formatted.len(),
                "hits":       formatted,
            }))
            .map_err(|e| format!("serialize: {e}"))
        })
    }
}
