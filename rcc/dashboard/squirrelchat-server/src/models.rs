use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Message ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub ts: i64,
    pub from_agent: String,
    pub text: String,
    pub channel: String,
    pub mentions: Vec<String>,
    pub thread_id: Option<i64>,
    pub reply_count: i64,
    /// HashMap<emoji, Vec<agent_id>>
    pub reactions: HashMap<String, Vec<String>>,
    pub slash_result: Option<String>,
}

// ── Reaction ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: usize,
    pub agents: Vec<String>,
}

impl Reaction {
    /// Build a Reaction list from the HashMap format.
    pub fn from_map(map: &HashMap<String, Vec<String>>) -> Vec<Reaction> {
        let mut out: Vec<Reaction> = map
            .iter()
            .map(|(emoji, agents)| Reaction {
                emoji: emoji.clone(),
                count: agents.len(),
                agents: agents.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.emoji.cmp(&b.emoji));
        out
    }
}

// ── Channel ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub description: Option<String>,
}

// ── User / Agent ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub user_type: String,
    pub online: bool,
    pub status: String,
    pub last_seen: Option<i64>,
}

// ── Project ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

// ── Project File ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: i64,
    pub filename: String,
    pub size: Option<i64>,
    pub encoding: String,
    pub created_at: i64,
}

// ── WS frames (server → client) ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Message { message: Message },
    Reaction { message_id: i64, reactions: Vec<Reaction> },
    Presence { agent: String, online: bool },
    Channel { action: String, channel: Channel },
    Connected { session_id: String },
}

// ── WS frames (client → server) ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Ping,
    Heartbeat { agent: String, status: String },
}
