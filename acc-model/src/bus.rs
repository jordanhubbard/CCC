//! Bus messages — the shape used by `/api/bus/send`, `/api/bus/messages`,
//! and the SSE stream at `/api/bus/stream`.
//!
//! The wire field for message kind is `type` (a Rust keyword), so we
//! rename it to [`BusMsg::kind`] in Rust. Everything else rides through
//! directly, with unknown fields captured in `extra`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BusMsg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<DateTime<Utc>>,

    /// Wire field: `type`. Examples: `tasks:added`, `tasks:claimed`,
    /// `text`, `reaction`, `projects:registered`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,

    /// MIME type (for binary bodies). When set, `enc` is typically `base64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enc: Option<String>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// POST /api/bus/send body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BusSendRequest {
    /// Wire field: `type`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enc: Option<String>,
    /// Caller-supplied arbitrary fields. Flattened into the top-level object.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_msg_kind_roundtrips_through_type_field() {
        let json = r#"{"type":"tasks:claimed","from":"agent-a","data":{"task_id":"t-1"}}"#;
        let m: BusMsg = serde_json::from_str(json).unwrap();
        assert_eq!(m.kind.as_deref(), Some("tasks:claimed"));
        assert_eq!(m.from.as_deref(), Some("agent-a"));
        let back = serde_json::to_string(&m).unwrap();
        assert!(back.contains("\"type\":\"tasks:claimed\""));
    }
}
