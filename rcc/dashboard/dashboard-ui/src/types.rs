use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

/// Deserialize priority as either a string ("high", "idea") or a number (85, 90).
/// Numbers are converted to their string representation.
fn deserialize_priority_flexible<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde_json::Value;
    let v: Option<Value> = Option::deserialize(deserializer)?;
    Ok(match v {
        None => None,
        Some(Value::String(s)) => Some(s),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(other) => Some(other.to_string()),
    })
}

// ── Heartbeat / Agent ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeartbeatData {
    pub agent: Option<String>,
    pub ts: Option<String>,
    pub host: Option<String>,
    pub model: Option<String>,
    pub online: Option<bool>,
    pub decommissioned: Option<bool>,
    pub status: Option<String>,
}

pub type HeartbeatMap = HashMap<String, HeartbeatData>;

// ── Agent Registry (from /api/agents) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    pub claude_cli: Option<bool>,
    pub claude_cli_model: Option<String>,
    pub inference_key: Option<bool>,
    pub inference_provider: Option<String>,
    pub gpu: Option<bool>,
    pub gpu_model: Option<String>,
    pub gpu_count: Option<u32>,
    pub gpu_vram_gb: Option<u32>,
    pub vllm: Option<bool>,
    pub vllm_port: Option<u32>,
    pub vllm_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentLlm {
    pub base_url: Option<String>,
    pub backend: Option<String>,
    pub models: Option<Vec<String>>,
    pub model_count: Option<u32>,
    pub fresh: Option<bool>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub name: Option<String>,
    pub host: Option<String>,
    #[serde(rename = "type")]
    pub agent_type: Option<String>,
    pub registered_at: Option<String>,
    pub last_seen: Option<String>,
    pub online_status: Option<String>,
    pub capabilities: Option<AgentCapabilities>,
    pub llm: Option<AgentLlm>,
}

pub type AgentList = Vec<AgentInfo>;

// ── Work Queue ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_priority_flexible")]
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub status: Option<String>,
    /// RCC API sends "created" (not "createdAt") — accept both
    #[serde(alias = "created")]
    pub created_at: Option<String>,
    /// Also accept "description" or "notes" as body fallback
    #[serde(alias = "description", alias = "notes")]
    pub body: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    pub claimed_by: Option<String>,
    pub resolution: Option<String>,
    /// Completion result text (some items use "result" instead of "resolution")
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueueResponse {
    pub items: Vec<QueueItem>,
    pub completed: Option<Vec<QueueItem>>,
}

// ── ClawBus ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BusMessage {
    pub id: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub text: Option<String>,
    pub ts: Option<String>,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
}

// ── Git Commits ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitCommit {
    pub sha: Option<String>,
    pub commit: Option<GitCommitDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitCommitDetail {
    pub message: Option<String>,
    pub author: Option<GitCommitAuthor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitCommitAuthor {
    pub name: Option<String>,
    pub date: Option<String>,
}

// ── GitHub Issues ────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhIssue {
    pub id: i64,
    pub repo: String,
    pub title: String,
    pub state: String,
    pub labels: Option<String>,
    pub url: Option<String>,
    pub author: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub wq_id: Option<String>,
    pub milestone: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GhIssuesResponse {
    pub ok: Option<bool>,
    pub issues: Vec<GhIssue>,
    pub count: Option<u32>,
    pub last_sync: Option<SyncLog>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncLog {
    pub repo: Option<String>,
    pub synced_at: Option<String>,
    pub count: Option<u32>,
    pub status: Option<String>,
}

// ── Projects ─────────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    pub id: String,
    pub display_name: Option<String>,
    pub enabled: Option<bool>,
}

// ── Metrics ──────────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricsData {
    pub queue_depth: Option<u32>,
    pub completion_rate: Option<f64>,
    pub active_agents: Option<u32>,
    pub pending: Option<u32>,
    pub in_progress: Option<u32>,
    pub completed: Option<u32>,
}
