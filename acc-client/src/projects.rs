//! Project operations on `/api/projects`.

use crate::{Client, Error, Result};
use acc_model::{CreateProjectRequest, Project};
use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
pub struct ProjectsApi<'a> {
    pub(crate) client: &'a Client,
}

impl<'a> ProjectsApi<'a> {
    pub fn list(self) -> ListProjectsBuilder<'a> {
        ListProjectsBuilder { client: self.client, status: None, q: None, limit: None }
    }

    /// GET /api/projects/{id}
    pub async fn get(self, id: &str) -> Result<Project> {
        let resp = self
            .client
            .http()
            .get(self.client.url(&format!("/api/projects/{id}")))
            .send()
            .await?;
        decode_single(resp).await
    }

    /// POST /api/projects
    pub async fn create(self, req: &CreateProjectRequest) -> Result<Project> {
        let resp = self
            .client
            .http()
            .post(self.client.url("/api/projects"))
            .json(req)
            .send()
            .await?;
        decode_single(resp).await
    }

    /// DELETE /api/projects/{id}
    ///
    /// `hard = true` requests a hard-delete; default is soft-archive.
    pub async fn delete(self, id: &str, hard: bool) -> Result<()> {
        let mut q: Vec<(&str, &str)> = Vec::new();
        if hard {
            q.push(("hard", "true"));
        }
        let resp = self
            .client
            .http()
            .delete(self.client.url(&format!("/api/projects/{id}")))
            .query(&q)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let bytes = resp.bytes().await?;
        Err(Error::from_response(status, &bytes))
    }
}

#[derive(Debug)]
pub struct ListProjectsBuilder<'a> {
    client: &'a Client,
    status: Option<String>,
    q: Option<String>,
    limit: Option<u32>,
}

impl<'a> ListProjectsBuilder<'a> {
    pub fn status(mut self, s: impl Into<String>) -> Self {
        self.status = Some(s.into());
        self
    }
    pub fn query(mut self, q: impl Into<String>) -> Self {
        self.q = Some(q.into());
        self
    }
    pub fn limit(mut self, n: u32) -> Self {
        self.limit = Some(n);
        self
    }

    pub async fn send(self) -> Result<Vec<Project>> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(s) = self.status {
            q.push(("status", s));
        }
        if let Some(qq) = self.q {
            q.push(("q", qq));
        }
        if let Some(n) = self.limit {
            q.push(("limit", n.to_string()));
        }
        let resp = self
            .client
            .http()
            .get(self.client.url("/api/projects"))
            .query(&q)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, &bytes));
        }
        let env: ListEnvelope = serde_json::from_slice(&bytes)?;
        Ok(match env {
            ListEnvelope::Wrapped { projects, .. } => projects,
            ListEnvelope::Bare(v) => v,
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ListEnvelope {
    Wrapped {
        projects: Vec<Project>,
        #[allow(dead_code)]
        #[serde(default)]
        total: Option<u64>,
    },
    Bare(Vec<Project>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SingleEnvelope {
    Ok { project: Project },
    Wrapped { project: Project },
    Bare(Project),
}

async fn decode_single(resp: reqwest::Response) -> Result<Project> {
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await?;
    if !(200..300).contains(&status) {
        return Err(Error::from_response(status, &bytes));
    }
    let env: SingleEnvelope = serde_json::from_slice(&bytes)?;
    Ok(match env {
        SingleEnvelope::Ok { project } | SingleEnvelope::Wrapped { project } => project,
        SingleEnvelope::Bare(p) => p,
    })
}
