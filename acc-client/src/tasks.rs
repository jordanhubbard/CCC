//! Task operations on `/api/tasks`.

use crate::{Client, Error, Result};
use acc_model::{
    ClaimRequest, CompleteRequest, CreateTaskRequest, ReviewResult, ReviewResultRequest, Task,
    TaskStatus, TaskType, UnclaimRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Entry point for task operations. Obtain via [`Client::tasks`].
#[derive(Debug, Clone, Copy)]
pub struct TasksApi<'a> {
    pub(crate) client: &'a Client,
}

impl<'a> TasksApi<'a> {
    pub fn list(self) -> ListTasksBuilder<'a> {
        ListTasksBuilder {
            client: self.client,
            status: None,
            task_type: None,
            project: None,
            agent: None,
            limit: None,
        }
    }

    /// GET /api/tasks/{id}
    pub async fn get(self, id: &str) -> Result<Task> {
        let resp = self
            .client
            .http()
            .get(self.client.url(&format!("/api/tasks/{id}")))
            .send()
            .await?;
        decode_single(resp).await
    }

    /// POST /api/tasks — returns the created task.
    pub async fn create(self, req: &CreateTaskRequest) -> Result<Task> {
        let resp = self
            .client
            .http()
            .post(self.client.url("/api/tasks"))
            .json(req)
            .send()
            .await?;
        decode_single(resp).await
    }

    /// PUT /api/tasks/{id}/claim
    ///
    /// Returns [`Error::Conflict`] if another agent has already claimed the
    /// task, or [`Error::Locked`] if the task has unfulfilled dependencies.
    pub async fn claim(self, id: &str, agent: &str) -> Result<Task> {
        let body = ClaimRequest {
            agent: agent.to_string(),
        };
        let resp = self
            .client
            .http()
            .put(self.client.url(&format!("/api/tasks/{id}/claim")))
            .json(&body)
            .send()
            .await?;
        decode_single(resp).await
    }

    /// PUT /api/tasks/{id}/unclaim
    pub async fn unclaim(self, id: &str, agent: Option<&str>) -> Result<()> {
        let body = UnclaimRequest {
            agent: agent.map(str::to_string),
        };
        let resp = self
            .client
            .http()
            .put(self.client.url(&format!("/api/tasks/{id}/unclaim")))
            .json(&body)
            .send()
            .await?;
        check_ok(resp).await
    }

    /// PUT /api/tasks/{id}/complete
    pub async fn complete(self, id: &str, agent: Option<&str>, output: Option<&str>) -> Result<()> {
        let body = CompleteRequest {
            agent: agent.map(str::to_string),
            output: output.map(str::to_string),
        };
        let resp = self
            .client
            .http()
            .put(self.client.url(&format!("/api/tasks/{id}/complete")))
            .json(&body)
            .send()
            .await?;
        check_ok(resp).await
    }

    /// PUT /api/tasks/{id}/review-result
    pub async fn review_result(
        self,
        id: &str,
        result: ReviewResult,
        agent: Option<&str>,
        notes: Option<&str>,
    ) -> Result<()> {
        let body = ReviewResultRequest {
            result,
            agent: agent.map(str::to_string),
            notes: notes.map(str::to_string),
        };
        let resp = self
            .client
            .http()
            .put(self.client.url(&format!("/api/tasks/{id}/review-result")))
            .json(&body)
            .send()
            .await?;
        check_ok(resp).await
    }

    /// DELETE /api/tasks/{id} — cancel/abort.
    pub async fn cancel(self, id: &str) -> Result<()> {
        let resp = self
            .client
            .http()
            .delete(self.client.url(&format!("/api/tasks/{id}")))
            .send()
            .await?;
        check_ok(resp).await
    }

    pub async fn vote(
        self,
        id: &str,
        agent: &str,
        vote: &str,
        refinement: &str,
    ) -> Result<()> {
        let body = serde_json::json!({
            "agent": agent,
            "vote": vote,
            "refinement": refinement,
        });
        let resp = self
            .client
            .http()
            .put(self.client.url(&format!("/api/tasks/{id}/vote")))
            .json(&body)
            .send()
            .await?;
        check_ok(resp).await
    }
}

/// Builder for GET /api/tasks. Construct via [`TasksApi::list`].
#[derive(Debug)]
pub struct ListTasksBuilder<'a> {
    client: &'a Client,
    status: Option<TaskStatus>,
    task_type: Option<TaskType>,
    project: Option<String>,
    agent: Option<String>,
    limit: Option<u32>,
}

impl<'a> ListTasksBuilder<'a> {
    pub fn status(mut self, s: TaskStatus) -> Self {
        self.status = Some(s);
        self
    }
    pub fn task_type(mut self, t: TaskType) -> Self {
        self.task_type = Some(t);
        self
    }
    pub fn project(mut self, id: impl Into<String>) -> Self {
        self.project = Some(id.into());
        self
    }
    pub fn agent(mut self, name: impl Into<String>) -> Self {
        self.agent = Some(name.into());
        self
    }
    pub fn limit(mut self, n: u32) -> Self {
        self.limit = Some(n);
        self
    }

    pub async fn send(self) -> Result<Vec<Task>> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(s) = self.status {
            q.push(("status", serde_plain(&s)));
        }
        if let Some(t) = self.task_type {
            q.push(("task_type", serde_plain(&t)));
        }
        if let Some(p) = self.project {
            q.push(("project_id", p));
        }
        if let Some(a) = self.agent {
            q.push(("agent", a));
        }
        if let Some(n) = self.limit {
            q.push(("limit", n.to_string()));
        }
        let resp = self
            .client
            .http()
            .get(self.client.url("/api/tasks"))
            .query(&q)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, &bytes));
        }
        let envelope: ListEnvelope = serde_json::from_slice(&bytes)?;
        Ok(envelope.tasks)
    }
}

// ── Response envelopes ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ListEnvelope {
    tasks: Vec<Task>,
}

/// Many write endpoints return `{ "task": { ... } }`, some return bare Task,
/// some return `{ "ok": true, "task": { ... } }`. Accept any of these.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SingleEnvelope {
    Wrapped { task: Task },
    Bare(Task),
}

async fn decode_single(resp: reqwest::Response) -> Result<Task> {
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await?;
    if !(200..300).contains(&status) {
        return Err(Error::from_response(status, &bytes));
    }
    let env: SingleEnvelope = serde_json::from_slice(&bytes)?;
    Ok(match env {
        SingleEnvelope::Wrapped { task } => task,
        SingleEnvelope::Bare(t) => t,
    })
}

async fn check_ok(resp: reqwest::Response) -> Result<()> {
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let bytes = resp.bytes().await?;
    Err(Error::from_response(status, &bytes))
}

/// Serialize a single enum value to its plain wire string (strip the JSON quotes).
fn serde_plain<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}
