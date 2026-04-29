//! LLM-callable tools that expose the ACC fleet task system.
//!
//! Restores the introspection capability the legacy Python "skills" surface
//! used to provide. Each bot can list tasks (with filters), fetch a specific
//! task, and check what is assigned to itself, all through the same public
//! API the CLI uses. Mutations (claim, complete, cancel) are deliberately
//! out of scope here — agents request work through the queue worker, not
//! by direct claim.

use super::tool::{Tool, ToolResult};
use acc_client::Client;
use acc_model::{Task, TaskStatus, TaskType};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;

fn parse_status(s: &str) -> Result<TaskStatus, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "open" => Ok(TaskStatus::Open),
        "claimed" => Ok(TaskStatus::Claimed),
        "in_progress" | "in-progress" => Ok(TaskStatus::InProgress),
        "completed" => Ok(TaskStatus::Completed),
        "cancelled" | "canceled" => Ok(TaskStatus::Cancelled),
        other => Err(format!(
            "unknown status '{other}'; valid: open|claimed|in_progress|completed|cancelled"
        )),
    }
}

fn parse_task_type(s: &str) -> Result<TaskType, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "work" => Ok(TaskType::Work),
        "review" => Ok(TaskType::Review),
        "idea" => Ok(TaskType::Idea),
        "discovery" => Ok(TaskType::Discovery),
        "phase_commit" | "phase-commit" => Ok(TaskType::PhaseCommit),
        "feature" => Ok(TaskType::Feature),
        "bug" => Ok(TaskType::Bug),
        "epic" => Ok(TaskType::Epic),
        "task" => Ok(TaskType::Task),
        other => Err(format!(
            "unknown task_type '{other}'; valid: work|review|idea|discovery|\
             phase_commit|feature|bug|epic|task"
        )),
    }
}

/// Slim a Task down to the fields a model will actually condition on.
/// Drops the metadata blob and timestamps that are not load-bearing for
/// "what's the work" / "who's on it" questions.
fn task_summary(t: &Task) -> Value {
    json!({
        "id":               t.id,
        "title":            t.title,
        "description":      t.description,
        "status":           t.status,
        "task_type":        t.task_type_str(),
        "priority":         t.priority,
        "project_id":       t.project_id,
        "claimed_by":       t.claimed_by,
        "preferred_agent":  t.preferred_agent,
        "assigned_agent":   t.assigned_agent,
        "preferred_executor": t.preferred_executor,
        "created_at":       t.created_at,
        "claimed_at":       t.claimed_at,
        "completed_at":     t.completed_at,
    })
}

// `task_type_str` is not on Task — Task::task_type is private to a `pub
// task_type: TaskType` field; we serialize via serde to grab the snake_case
// string. Doing this once per task in the helper above keeps the JSON output
// stable even when the Task struct grows new fields.
trait TaskTypeStr {
    fn task_type_str(&self) -> String;
}
impl TaskTypeStr for Task {
    fn task_type_str(&self) -> String {
        serde_json::to_value(self.task_type)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

// ── acc_tasks_list ────────────────────────────────────────────────────────────

pub struct AccTasksListTool {
    client: Arc<Client>,
}

impl AccTasksListTool {
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }
}

impl Tool for AccTasksListTool {
    fn name(&self) -> &str {
        "acc_tasks_list"
    }
    fn description(&self) -> &str {
        "List tasks in the ACC fleet task system. Optional filters: status \
         (open|claimed|in_progress|completed|cancelled), task_type \
         (work|review|idea|discovery|feature|bug|epic|task), project \
         (project ID), agent (assigned-agent name). limit defaults to 20, \
         hard-cap 100. Returns each task's id, title, status, type, \
         priority, project, and claim/assignment fields."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status":    {"type": "string", "description": "Filter to a single status"},
                "task_type": {"type": "string", "description": "Filter to a single task type"},
                "project":   {"type": "string", "description": "Filter to a project ID"},
                "agent":     {"type": "string", "description": "Filter to one assigned agent"},
                "limit":     {"type": "integer", "description": "Page size (1-100, default 20)"}
            }
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let mut b = self.client.tasks().list();
            if let Some(s) = input["status"].as_str().filter(|s| !s.is_empty()) {
                b = b.status(parse_status(s)?);
            }
            if let Some(t) = input["task_type"].as_str().filter(|s| !s.is_empty()) {
                b = b.task_type(parse_task_type(t)?);
            }
            if let Some(p) = input["project"].as_str().filter(|s| !s.is_empty()) {
                b = b.project(p.to_string());
            }
            if let Some(a) = input["agent"].as_str().filter(|s| !s.is_empty()) {
                b = b.agent(a.to_string());
            }
            let limit = input["limit"]
                .as_u64()
                .map(|v| v.clamp(1, MAX_LIMIT as u64) as u32)
                .unwrap_or(DEFAULT_LIMIT);
            b = b.limit(limit);

            let tasks = b.send().await.map_err(|e| format!("acc list: {e}"))?;
            let summaries: Vec<Value> = tasks.iter().map(task_summary).collect();
            serde_json::to_string_pretty(&json!({
                "count": summaries.len(),
                "tasks": summaries,
            }))
            .map_err(|e| format!("serialize: {e}"))
        })
    }
}

// ── acc_tasks_get ─────────────────────────────────────────────────────────────

pub struct AccTasksGetTool {
    client: Arc<Client>,
}

impl AccTasksGetTool {
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }
}

impl Tool for AccTasksGetTool {
    fn name(&self) -> &str {
        "acc_tasks_get"
    }
    fn description(&self) -> &str {
        "Fetch one ACC task by its ID. Returns the full task: title, \
         description, status, type, priority, project, all claim and \
         assignment fields, and timestamps."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Task ID"}
            },
            "required": ["id"]
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let id = input["id"].as_str().unwrap_or("").trim();
            if id.is_empty() {
                return Err("id is required".to_string());
            }
            let task = self
                .client
                .tasks()
                .get(id)
                .await
                .map_err(|e| format!("acc get {id}: {e}"))?;
            // Full task object — for `get` we want everything, not just the
            // condensed summary the list view returns.
            serde_json::to_string_pretty(&task).map_err(|e| format!("serialize: {e}"))
        })
    }
}

// ── acc_tasks_mine ────────────────────────────────────────────────────────────

pub struct AccTasksMineTool {
    client: Arc<Client>,
    agent_name: String,
}

impl AccTasksMineTool {
    pub fn new(client: Arc<Client>, agent_name: String) -> Self {
        Self { client, agent_name }
    }
}

impl Tool for AccTasksMineTool {
    fn name(&self) -> &str {
        "acc_tasks_mine"
    }
    fn description(&self) -> &str {
        "List tasks claimed by, assigned to, or preferred for this agent. \
         Convenience wrapper for `acc_tasks_list` filtered to the current \
         agent's name."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": "Optional status filter"},
                "limit":  {"type": "integer", "description": "Page size (1-100, default 20)"}
            }
        })
    }
    fn execute<'a>(
        &'a self,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let mut b = self.client.tasks().list().agent(self.agent_name.clone());
            if let Some(s) = input["status"].as_str().filter(|s| !s.is_empty()) {
                b = b.status(parse_status(s)?);
            }
            let limit = input["limit"]
                .as_u64()
                .map(|v| v.clamp(1, MAX_LIMIT as u64) as u32)
                .unwrap_or(DEFAULT_LIMIT);
            b = b.limit(limit);

            let tasks = b.send().await.map_err(|e| format!("acc list: {e}"))?;
            let summaries: Vec<Value> = tasks.iter().map(task_summary).collect();
            serde_json::to_string_pretty(&json!({
                "agent":  self.agent_name,
                "count":  summaries.len(),
                "tasks":  summaries,
            }))
            .map_err(|e| format!("serialize: {e}"))
        })
    }
}

/// Construct the full ACC-task tool set sharing one client.
pub fn all_acc_task_tools(client: Arc<Client>, agent_name: String) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(AccTasksListTool::new(client.clone())),
        Box::new(AccTasksGetTool::new(client.clone())),
        Box::new(AccTasksMineTool::new(client, agent_name)),
    ]
}
