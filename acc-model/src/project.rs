use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectStatus {
    Active,
    Archived,
}

/// Project as emitted by `/api/projects` and `/api/projects/{id}`.
///
/// Like [`crate::QueueItem`], unknown fields land in `extra` so the client
/// survives server additions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProjectStatus>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_url: Option<String>,
    #[serde(default, rename = "repoUrl", skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agentfs_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_status: Option<String>,

    #[serde(default, rename = "slackChannels", skip_serializing_if = "Vec::is_empty")]
    pub slack_channels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    #[serde(default, rename = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,

    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_preserves_unknown_fields() {
        let json = r#"{
            "id": "proj-1",
            "name": "demo",
            "slug": "demo",
            "status": "active",
            "createdAt": "2026-04-23T00:00:00Z",
            "futureField": 42
        }"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(p.id, "proj-1");
        assert_eq!(p.status, Some(ProjectStatus::Active));
        assert!(p.extra.contains_key("futureField"));
    }
}
