use serde::{Deserialize, Serialize};

/// A ClawBus message (text type used for chat).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BusMessage {
    pub id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub ts: Option<String>,
    pub seq: Option<i64>,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub body: Option<String>,
    pub subject: Option<String>,
    pub mime: Option<String>,
}

/// A channel in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub id: String,   // e.g. "#general"
    pub label: String,
}

/// An agent's presence entry from /bus/presence.
#[derive(Clone, Debug, PartialEq)]
pub struct PresenceEntry {
    pub agent: String,
    pub online: bool,
}

/// The shared app context passed down to all components.
#[derive(Clone, Copy)]
pub struct ChatContext {
    pub token: leptos::ReadSignal<Option<String>>,
    pub set_token: leptos::WriteSignal<Option<String>>,
    pub username: leptos::ReadSignal<String>,
    pub messages: leptos::ReadSignal<Vec<BusMessage>>,
    pub set_messages: leptos::WriteSignal<Vec<BusMessage>>,
    pub active_channel: leptos::ReadSignal<String>,
    pub set_active_channel: leptos::WriteSignal<String>,
    pub presence: leptos::ReadSignal<Vec<PresenceEntry>>,
    pub connected: leptos::ReadSignal<bool>,
    /// Per-channel read watermarks: channel_id → count of messages seen.
    /// Unread = current_count - read_counts[ch]. If missing, defaults to current (0 unread).
    pub read_counts: leptos::RwSignal<std::collections::HashMap<String, usize>>,
}

/// Default channels always shown in the sidebar.
pub const DEFAULT_CHANNELS: &[(&str, &str)] = &[
    ("#general", "general"),
    ("#ops",     "ops"),
    ("#ai",      "ai"),
    ("#data",    "data"),
];
