// sc_types.rs — SquirrelChat shared type definitions
// Single source of truth. Both squirrelchat.rs and future component modules import from here.
// Wire format matches squirrelchat-server (Rust/Axum) models.rs exactly.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Message ─────────────────────────────────────────────────────────────────

/// Wire format from squirrelchat-server's `Message` struct.
/// `reactions` is `HashMap<emoji, Vec<agent_id>>` — e.g. `{"🔥": ["rocky", "natasha"]}`.
/// `from_agent` is the user/agent id.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScMessage {
    pub id: Option<i64>,
    /// unix ms
    #[serde(default)]
    pub ts: i64,
    /// user/agent id (field name is `from_agent` on the wire)
    pub from_agent: Option<String>,
    pub text: Option<String>,
    pub channel: Option<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
    /// parent message id if this is a thread reply
    pub thread_id: Option<i64>,
    /// number of replies on a top-level message
    #[serde(default)]
    pub reply_count: i64,
    /// emoji → list of agent ids who reacted
    #[serde(default)]
    pub reactions: HashMap<String, Vec<String>>,
    /// legacy slash command result field
    pub slash_result: Option<String>,
}

impl ScMessage {
    /// Returns true if the given user has reacted with this emoji.
    pub fn user_reacted(&self, user_id: &str, emoji: &str) -> bool {
        self.reactions
            .get(emoji)
            .map(|users| users.iter().any(|u| u == user_id))
            .unwrap_or(false)
    }

    /// Returns a sorted vec of (emoji, count) pairs for display.
    pub fn reaction_counts(&self) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = self
            .reactions
            .iter()
            .map(|(emoji, users)| (emoji.clone(), users.len()))
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        counts
    }

    /// Format the message timestamp as HH:MM:SS for display.
    pub fn format_ts(&self) -> String {
        let secs = (self.ts / 1000) as u64;
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }
}

// ─── Reaction (aggregated form, used in WS Reaction frames) ──────────────────

/// The `Reaction` struct from the server's WS `Reaction` frame payload.
/// Contains both count and full agent list for hover tooltips.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScReaction {
    pub emoji: String,
    pub count: usize,
    pub agents: Vec<String>,
}

// ─── Channel ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScChannel {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// "public" | "dm" — wire field is `type`
    #[serde(rename = "type")]
    pub channel_type: Option<String>,
    pub created_by: Option<String>,
    pub created_at: Option<i64>,
    /// Local-only: unread count (not from wire, tracked client-side)
    #[serde(skip)]
    pub unread_count: u32,
}

// ─── User / Agent ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScUser {
    pub id: String,
    pub name: String,
    /// "agent" | "human" — wire field is `type`
    #[serde(rename = "type", default)]
    pub user_type: Option<String>,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub status: String,
    pub last_seen: Option<i64>,
}

impl ScUser {
    pub fn presence_icon(&self) -> &'static str {
        if self.online {
            match self.status.as_str() {
                "idle" => "🟡",
                _ => "🟢",
            }
        } else {
            "🔴"
        }
    }
}

/// Legacy alias — kept until all callers migrate to ScUser
pub type ScAgent = ScUser;

// ─── Identity (current user) ──────────────────────────────────────────────────

/// Returned by GET /api/me, also stored in localStorage as "sc_identity"
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScIdentity {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub avatar_url: Option<String>,
    /// If true, the user has no name set yet — show the "set your name" modal
    #[serde(default)]
    pub needs_name: bool,
    /// Local-only: auth token (not returned by server, stored separately)
    #[serde(skip)]
    pub token: Option<String>,
}

// ─── WebSocket frames ────────────────────────────────────────────────────────

/// Client → Server frames (matching squirrelchat-server's `ClientFrame`)
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScWsClientFrame {
    Ping,
    Heartbeat { agent: String, status: String },
}

/// Server → Client frames (matching squirrelchat-server's `ServerFrame`)
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScWsFrame {
    /// New message or thread reply
    Message { message: ScMessage },
    /// Reaction updated on a message
    Reaction {
        message_id: i64,
        reactions: Vec<ScReaction>,
    },
    /// Agent presence change
    Presence { agent: String, online: bool },
    /// Channel created/updated/deleted
    Channel { action: String, channel: ScChannel },
    /// Initial connection confirmation
    Connected { session_id: String },
}

// ─── Project ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScProject {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub assignee: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

// ─── File ────────────────────────────────────────────────────────────────────

/// Project file info (from GET /api/projects/:id/files)
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScFile {
    pub id: Option<i64>,
    pub filename: String,
    pub size: Option<i64>,
    pub encoding: Option<String>,
    pub created_at: Option<i64>,
}

// ─── Fallbacks (used before dynamic data loads) ───────────────────────────────

pub const DEFAULT_CHANNELS: &[(&str, &str)] = &[
    ("general", "General"),
    ("agents", "Agents"),
    ("ops", "Ops"),
    ("random", "Random"),
];

pub const FALLBACK_AGENT_NAMES: &[&str] =
    &["natasha", "rocky", "bullwinkle", "sparky", "boris"];

/// The emoji palette for the reaction picker (small curated set)
pub const REACTION_EMOJIS: &[&str] = &["👍", "❤️", "😂", "🔥", "👀", "🎉", "🤔", "✅"];
