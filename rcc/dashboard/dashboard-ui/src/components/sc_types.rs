// sc_types.rs — ClawChat shared type definitions
// Single source of truth. Both squirrelchat.rs and future component modules import from here.
// Wire format matches squirrelchat-server (Rust/Axum) models.rs exactly.

use serde::{Deserialize, Serialize};

// ─── Message ─────────────────────────────────────────────────────────────────

/// Wire format from squirrelchat-server's `MessageWire` struct.
/// `reactions` is `Vec<ScReaction>` — aggregated `{emoji, count, agents}`.
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
    /// Aggregated reactions: [{emoji, count, agents}]
    #[serde(default)]
    pub reactions: Vec<ScReaction>,
    /// File attachments on this message
    #[serde(default)]
    pub attachments: Vec<ScAttachment>,
    /// Whether this message is pinned in its channel (client-side only, not from wire)
    #[serde(skip)]
    pub is_pinned: bool,
    /// legacy slash command result field
    pub slash_result: Option<String>,
}

// ─── Attachment ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScAttachment {
    pub id: i64,
    pub message_id: i64,
    pub filename: String,
    pub mime_type: String,
    pub size: Option<i64>,
    pub created_at: i64,
}

#[allow(dead_code)]
impl ScMessage {
    /// Returns true if the given user has reacted with this emoji.
    pub fn user_reacted(&self, user_id: &str, emoji: &str) -> bool {
        self.reactions
            .iter()
            .any(|r| r.emoji == emoji && r.agents.iter().any(|u| u == user_id))
    }

    /// Returns reactions sorted by count descending for display.
    pub fn sorted_reactions(&self) -> Vec<&ScReaction> {
        let mut sorted: Vec<&ScReaction> = self.reactions.iter().collect();
        sorted.sort_by(|a, b| b.count.cmp(&a.count).then(a.emoji.cmp(&b.emoji)));
        sorted
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

#[allow(dead_code)]
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
#[allow(dead_code)]
pub type ScAgent = ScUser;

// ─── Presence ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScPresenceEntry {
    /// "online" | "away" | "offline" | "unknown"
    pub status: String,
    #[serde(rename = "statusText", default)]
    pub status_text: String,
    pub since: Option<i64>,
    pub host: Option<String>,
    pub gpu: Option<String>,
    #[serde(default)]
    pub gap_minutes: Option<i64>,
}

impl ScPresenceEntry {
    /// CSS class name for status dot
    pub fn dot_class(&self) -> &'static str {
        match self.status.as_str() {
            "online"  => "sc-presence-dot sc-presence-online",
            "away"    => "sc-presence-dot sc-presence-away",
            "offline" => "sc-presence-dot sc-presence-offline",
            _         => "sc-presence-dot sc-presence-unknown",
        }
    }
    /// Emoji fallback for non-CSS contexts
    pub fn icon(&self) -> &'static str {
        match self.status.as_str() {
            "online"  => "🟢",
            "away"    => "🟡",
            "offline" => "🔴",
            _         => "⚫",
        }
    }
}

/// The full presence map returned by /api/presence
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct ScPresenceMap {
    #[serde(default)]
    pub ok: bool,
    /// agent_name → presence entry
    pub agents: std::collections::HashMap<String, ScPresenceEntry>,
    pub ts: Option<String>,
}

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
#[allow(dead_code)]
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
    /// Transient typing indicator
    Typing { channel: String, agent: String, is_typing: bool },
    /// Unread counts refresh signal — empty counts = "please re-fetch your own"
    UnreadUpdate {
        #[serde(default)]
        counts: std::collections::HashMap<String, i64>,
    },
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
