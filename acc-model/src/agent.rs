use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Computed liveness classification emitted by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOnlineStatus {
    Online,
    Offline,
    Decommissioned,
}

/// Agent as emitted by `/api/agents` and `/api/agents/{name}`.
///
/// The server emits ~30 fields, many of which are GPU / VRAM telemetry
/// that's only meaningful for GPU nodes. We model the registry fields
/// strongly and funnel telemetry and future additions through `extra`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Agent {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,

    /// "full" | "partial"
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ccc_version: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vllm_port: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slack_id: Option<String>,

    /// Only present in responses to the agent itself, or registry endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    #[serde(default, rename = "registeredAt", skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "lastSeen", skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<DateTime<Utc>>,

    /// Capabilities can arrive as an array of strings or a map — we take
    /// the raw value and let callers decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    #[serde(default, rename = "onlineStatus", skip_serializing_if = "Option::is_none")]
    pub online_status: Option<AgentOnlineStatus>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decommissioned: Option<bool>,
    #[serde(default, rename = "decommissionedAt", skip_serializing_if = "Option::is_none")]
    pub decommissioned_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u64>,

    /// GPU/VRAM/RAM telemetry, ollama status, billing tier, etc.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_registers_core_fields_and_captures_telemetry() {
        let json = r#"{
            "name": "natasha",
            "host": "natasha.local",
            "type": "full",
            "lastSeen": "2026-04-23T00:00:00Z",
            "online": true,
            "onlineStatus": "online",
            "gpu": true,
            "gpu_temp_c": 54.3,
            "vram_used_mb": 1024
        }"#;
        let a: Agent = serde_json::from_str(json).unwrap();
        assert_eq!(a.name, "natasha");
        assert_eq!(a.online, Some(true));
        assert_eq!(a.online_status, Some(AgentOnlineStatus::Online));
        assert!(a.extra.contains_key("gpu_temp_c"));
        assert!(a.extra.contains_key("vram_used_mb"));
    }
}
