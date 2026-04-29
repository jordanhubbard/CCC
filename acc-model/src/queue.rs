//! Queue items and related request bodies.
//!
//! The queue's wire format is historically loose — some fields are
//! camelCase, a few are snake_case, and `status` can arrive as both
//! `in-progress` and `in_progress`. We model the fields that callers
//! actually branch on as strongly-typed optionals, and capture any
//! unknown fields in [`QueueItem::extra`] via `#[serde(flatten)]` so
//! server additions don't crash deserialization.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::agent::{AgentExecutor, AgentSession};

/// Queue item as emitted by `/api/queue` and `/api/item/{id}`.
///
/// Unknown or not-yet-modeled fields land in [`QueueItem::extra`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueueItem {
    pub id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    /// "pending" | "in_progress" | "in-progress" | "claimed" | "completed" |
    /// "cancelled" | "blocked" | "incubating" — kept as a string so unknown
    /// future values don't break deserialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,

    /// "critical" | "high" | "medium" | "normal" | "low" | "idea" | "urgent"
    /// — observed values, modeled as a string for forward-compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_executor: Option<String>,

    #[serde(default, rename = "claimedBy", skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,

    #[serde(default, rename = "claimedAt", skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<DateTime<Utc>>,

    #[serde(
        default,
        rename = "completedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub completed_at: Option<DateTime<Utc>>,

    #[serde(
        default,
        rename = "keepaliveAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub keepalive_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u64>,

    #[serde(
        default,
        rename = "maxAttempts",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_attempts: Option<u64>,

    #[serde(
        default,
        rename = "blockedReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub blocked_reason: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Every field we don't model explicitly — `journal`, `choices`,
    /// `votes`, `events`, `repo`, `project`, `branch`, and any future
    /// additions.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

// ── Request bodies ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimItemRequest {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteItemRequest {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailItemRequest {
    pub agent: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentItemRequest {
    pub agent: String,
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeepaliveRequest {
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// POST /api/heartbeat/{agent} — agent liveness beacon.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u64>,
    /// How many tasks this agent is actively executing right now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks_in_flight: Option<u32>,
    /// Self-reported free capacity (how many more tasks this agent can accept).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_free_slots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_session_slots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sessions: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_spawn_denied_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executors: Vec<AgentExecutor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<AgentSession>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_item_captures_unknown_fields_in_extra() {
        let json = r#"{
            "id": "wq-1",
            "title": "do thing",
            "status": "pending",
            "priority": "normal",
            "assignee": "all",
            "tags": ["gpu"],
            "maxAttempts": 3,
            "journal": [{"ts": "2026-04-23T00:00:00Z", "text": "hi"}],
            "branch": "main"
        }"#;
        let item: QueueItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, "wq-1");
        assert_eq!(item.title.as_deref(), Some("do thing"));
        assert_eq!(item.max_attempts, Some(3));
        assert!(item.extra.contains_key("journal"));
        assert!(item.extra.contains_key("branch"));
    }

    #[test]
    fn status_accepts_both_spellings() {
        let hyphen: QueueItem =
            serde_json::from_str(r#"{"id":"1","status":"in-progress"}"#).unwrap();
        assert_eq!(hyphen.status.as_deref(), Some("in-progress"));
        let under: QueueItem =
            serde_json::from_str(r#"{"id":"1","status":"in_progress"}"#).unwrap();
        assert_eq!(under.status.as_deref(), Some("in_progress"));
    }
}
