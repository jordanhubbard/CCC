use serde::{Deserialize, Serialize};

// ── Tab state ─────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Overview,
    Kanban,
    Bus,
    Providers,
    Projects,
    Calendar,
    Audit,
    GeekView,
    Settings,
}

impl Tab {
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Overview  => "Overview",
            Tab::Kanban    => "Kanban",
            Tab::Bus       => "SquirrelBus",
            Tab::Providers => "⚡ Providers",
            Tab::Projects  => "Projects",
            Tab::Calendar  => "Calendar",
            Tab::Audit     => "🔍 Audit",
            Tab::GeekView  => "🖥️ Geek View",
            Tab::Settings  => "⚙️ Settings",
        }
    }
}

// ── Cap audit event (mirrors cap_audit_entry_t from cap_audit_log.c) ─────────
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapEvent {
    pub seq:        u64,
    pub ts:         u64,
    pub tick:       String,
    pub event_type: String,
    pub slot_id:    u32,
    pub agent_id:   u32,
    pub caps_mask:  String,
    pub caps_names: Vec<String>,
}

// ── Cap events response ───────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapEventsResponse {
    pub events:        Vec<CapEvent>,
    pub total_in_ring: u64,
    pub slots:         Vec<u32>,
    pub event_types:   Vec<String>,
    pub cap_classes:   Vec<String>,
    pub generated_at:  u64,
}

// ── Queue item ────────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(rename = "type", default)]
    pub item_type: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub needs_human: bool,
    #[serde(default)]
    pub needs_human_reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
}

fn default_status() -> String {
    "pending".to_string()
}

impl QueueItem {
    /// Derive item type from tags or explicit type field.
    pub fn card_type(&self) -> &str {
        if let Some(ref t) = self.item_type {
            return t.as_str();
        }
        for tag in &self.tags {
            match tag.as_str() {
                "bug"      => return "bug",
                "idea"     => return "idea",
                "feature"  => return "feature",
                "proposal" => return "proposal",
                _ => {}
            }
        }
        "task"
    }

    pub fn assignee_or_unassigned(&self) -> &str {
        self.assignee.as_deref().unwrap_or("unassigned")
    }

    pub fn priority_str(&self) -> &str {
        self.priority.as_deref().unwrap_or("medium")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub ts: String,
    pub author: String,
    pub text: String,
}

// ── Heartbeat ─────────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Heartbeat {
    pub agent: String,
    pub ts: Option<String>,
    pub host: Option<String>,
    pub activity: Option<String>,
    #[serde(rename = "activitySince")]
    pub activity_since: Option<String>,
    pub status: Option<String>,
}

/// Map of agent name → Heartbeat, from /api/heartbeats
pub type HeartbeatMap = std::collections::HashMap<String, Heartbeat>;

impl Heartbeat {
    /// Age in seconds (None if ts missing or unparseable).
    pub fn age_secs(&self) -> Option<f64> {
        let ts = self.ts.as_deref()?;
        // Simple JS Date.parse equivalent via js_sys
        let ms = js_sys::Date::parse(ts);
        if ms.is_nan() {
            return None;
        }
        let now = js_sys::Date::now();
        Some((now - ms) / 1000.0)
    }

    pub fn status_class(&self) -> &'static str {
        match self.age_secs() {
            Some(s) if s < 300.0  => "online",
            Some(s) if s < 1800.0 => "stale",
            _                      => "offline",
        }
    }

    pub fn age_label(&self) -> String {
        match self.age_secs() {
            None => "unknown".to_string(),
            Some(s) if s < 60.0  => format!("{}s ago", s as u64),
            Some(s) if s < 3600.0 => format!("{}m ago", (s / 60.0) as u64),
            Some(s) => format!("{}h ago", (s / 3600.0) as u64),
        }
    }
}

// ── Bus message ───────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BusMessage {
    pub id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub body: Option<String>,
    pub ts: Option<String>,
}

// ── Traffic event (from /api/geek/stream) ─────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrafficEvent {
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub ts: Option<String>,
}

// ── Project ───────────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(rename = "githubRepo", default)]
    pub github_repo: Option<String>,
}

// ── Calendar event ────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CalEvent {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub owner: Option<String>,
    #[serde(rename = "type", default)]
    pub event_type: Option<String>,
}

// ── Token Provider ────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub model: String,
    #[serde(rename = "baseUrl", default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub local_port: Option<u32>,
    #[serde(default = "default_provider_status")]
    pub status: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub context_len: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn default_provider_status() -> String {
    "unknown".to_string()
}

impl Provider {
    pub fn status_class(&self) -> &'static str {
        match self.status.as_str() {
            "online"  => "online",
            "offline" => "offline",
            _         => "stale",
        }
    }

    pub fn context_label(&self) -> String {
        match self.context_len {
            None => "—".to_string(),
            Some(n) if n >= 1_000_000 => format!("{}M ctx", n / 1_000_000),
            Some(n) if n >= 1_000     => format!("{}K ctx", n / 1_000),
            Some(n)                    => format!("{} ctx", n),
        }
    }
}

// ── Agent color/emoji helpers ─────────────────────────────────────────────────
pub fn agent_color(agent: &str) -> &'static str {
    match agent.to_lowercase().as_str() {
        "rocky"      => "#f85149",
        "bullwinkle" => "#a371f7",
        "natasha"    => "#3fb950",
        "boris"      => "#d29922",
        "jkh"        => "#ffd700",
        _            => "#8b949e",
    }
}

pub fn agent_emoji(agent: &str) -> &'static str {
    match agent.to_lowercase().as_str() {
        "rocky"      => "🐿️",
        "bullwinkle" => "🫎",
        "natasha"    => "🕵️",
        "boris"      => "⚡",
        "jkh"        => "👤",
        _            => "🤖",
    }
}
