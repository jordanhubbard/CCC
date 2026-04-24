use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Task lifecycle state as emitted by the server.
///
/// The server historically accepts both `in-progress` and `in_progress`
/// on *input*; on output it uses `in_progress`. We model only the output
/// values here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Claimed,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Work,
    Review,
    Idea,
    Discovery,
    PhaseCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewResult {
    Approved,
    Rejected,
}

macro_rules! impl_fromstr_via_serde {
    ($ty:ty) => {
        impl std::str::FromStr for $ty {
            type Err = serde_json::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                serde_json::from_value(serde_json::Value::String(s.to_string()))
            }
        }
    };
}

impl_fromstr_via_serde!(TaskStatus);
impl_fromstr_via_serde!(TaskType);
impl_fromstr_via_serde!(ReviewResult);

/// A fleet task.
///
/// Only `id` and `status` are strictly required; the server historically
/// emits tasks with partial fields (and some test harnesses do too), so
/// we default the rest. Callers that need a field which is semantically
/// required in their context should check after deserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_expires_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_by: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub metadata: Value,

    #[serde(default = "default_task_type")]
    pub task_type: TaskType,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,

    #[serde(default)]
    pub blocked_by: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_result: Option<ReviewResult>,
}

fn default_task_type() -> TaskType {
    TaskType::Work
}

// ── Request bodies ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateTaskRequest {
    pub project_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<TaskType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub agent: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnclaimRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompleteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResultRequest {
    pub result: ReviewResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_serializes_to_wire_form() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Open).unwrap(),
            "\"open\""
        );
    }

    #[test]
    fn task_type_phase_commit_roundtrips() {
        let wire = "\"phase_commit\"";
        let t: TaskType = serde_json::from_str(wire).unwrap();
        assert_eq!(t, TaskType::PhaseCommit);
        assert_eq!(serde_json::to_string(&t).unwrap(), wire);
    }

    #[test]
    fn task_deserializes_minimal_shape() {
        let json = r#"{
            "id": "task-1",
            "project_id": "proj-x",
            "title": "t",
            "description": "",
            "status": "open",
            "priority": 2,
            "created_at": "2026-04-23T00:00:00Z"
        }"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert_eq!(t.id, "task-1");
        assert_eq!(t.status, TaskStatus::Open);
        assert_eq!(t.task_type, TaskType::Work);
        assert!(t.claimed_by.is_none());
        assert!(t.blocked_by.is_empty());
    }

    #[test]
    fn api_error_preserves_extra_fields() {
        let json = r#"{"error":"blocked","pending":"task-9"}"#;
        let e: super::super::error::ApiError = serde_json::from_str(json).unwrap();
        assert_eq!(e.error, "blocked");
        assert_eq!(e.extra.get("pending").and_then(|v| v.as_str()), Some("task-9"));
    }
}
