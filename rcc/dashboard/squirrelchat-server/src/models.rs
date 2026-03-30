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
    /// Wire format: HashMap<emoji, Vec<agent_id>> stored; aggregated to Vec<Reaction> in WS frames
    #[serde(skip)]
    pub reactions_map: HashMap<String, Vec<String>>,
    pub slash_result: Option<String>,
}

// ── Reaction (wire type for WS frames and REST responses) ────────────────────

/// `{emoji, count, agents}` — matches ScReaction in sc_types.rs exactly.
#[derive(Debug, Clone, Serialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: usize,
    pub agents: Vec<String>,
}

impl Reaction {
    pub fn from_map(map: &HashMap<String, Vec<String>>) -> Vec<Reaction> {
        let mut out: Vec<Reaction> = map
            .iter()
            .map(|(emoji, agents)| Reaction {
                emoji: emoji.clone(),
                count: agents.len(),
                agents: agents.clone(),
            })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.emoji.cmp(&b.emoji)));
        out
    }
}

/// Message with reactions aggregated for wire (REST + WS responses).
#[derive(Debug, Clone, Serialize)]
pub struct MessageWire {
    pub id: i64,
    pub ts: i64,
    pub from_agent: String,
    pub text: String,
    pub channel: String,
    pub mentions: Vec<String>,
    pub thread_id: Option<i64>,
    pub reply_count: i64,
    pub reactions: Vec<Reaction>,
    pub slash_result: Option<String>,
    /// Inline attachment metadata (content fetched separately via /api/attachments/:id)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attachments: Vec<Attachment>,
}

impl From<Message> for MessageWire {
    fn from(m: Message) -> Self {
        let reactions = Reaction::from_map(&m.reactions_map);
        MessageWire {
            id: m.id, ts: m.ts, from_agent: m.from_agent, text: m.text,
            channel: m.channel, mentions: m.mentions, thread_id: m.thread_id,
            reply_count: m.reply_count, reactions, slash_result: m.slash_result,
            attachments: vec![],
        }
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

// ── Message Attachment ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: i64,
    pub message_id: i64,
    pub filename: String,
    pub mime_type: String,
    pub size: Option<i64>,
    pub created_at: i64,
}

// ── WS frames (server → client) ───────────────────────────────────────────────
// These match ScWsFrame in sc_types.rs exactly.

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// New message
    Message { message: MessageWire },
    /// Reaction updated — full aggregated list for the message
    Reaction { message_id: i64, reactions: Vec<Reaction> },
    /// Agent presence change
    Presence { agent: String, online: bool },
    /// Channel created
    Channel { action: String, channel: Channel },
    /// Connection confirmed
    Connected { session_id: String },
    /// Transient typing indicator relay
    Typing { channel: String, agent: String, is_typing: bool },
}

// ── WS frames (client → server) ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Ping,
    Heartbeat { agent: String, status: String },
    Typing { channel: String, agent: String, is_typing: bool },
}
