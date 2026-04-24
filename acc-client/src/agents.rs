//! Agent registry reads on `/api/agents`.

use crate::{Client, Error, Result};
use acc_model::Agent;
use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
pub struct AgentsApi<'a> {
    pub(crate) client: &'a Client,
}

impl<'a> AgentsApi<'a> {
    pub fn list(self) -> ListAgentsBuilder<'a> {
        ListAgentsBuilder { client: self.client, online: None }
    }

    /// GET /api/agents/names?online=...
    ///
    /// Lightweight list returning just agent names — useful for peer discovery
    /// where the full agent envelope would be wasteful.
    pub async fn names(self, online: bool) -> Result<Vec<String>> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if online {
            q.push(("online", "true".into()));
        }
        let resp = self
            .client
            .http()
            .get(self.client.url("/api/agents/names"))
            .query(&q)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, &bytes));
        }
        let env: NamesEnvelope = serde_json::from_slice(&bytes)?;
        Ok(match env {
            NamesEnvelope::Wrapped { names } => names,
            NamesEnvelope::Bare(v) => v,
        })
    }

    /// GET /api/agents/{name}
    pub async fn get(self, name: &str) -> Result<Agent> {
        let resp = self
            .client
            .http()
            .get(self.client.url(&format!("/api/agents/{name}")))
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, &bytes));
        }
        let env: SingleEnvelope = serde_json::from_slice(&bytes)?;
        Ok(match env {
            SingleEnvelope::Wrapped { agent } => agent,
            SingleEnvelope::Bare(a) => a,
        })
    }
}

#[derive(Debug)]
pub struct ListAgentsBuilder<'a> {
    client: &'a Client,
    online: Option<bool>,
}

impl<'a> ListAgentsBuilder<'a> {
    pub fn online(mut self, b: bool) -> Self {
        self.online = Some(b);
        self
    }

    pub async fn send(self) -> Result<Vec<Agent>> {
        let mut q: Vec<(&'static str, String)> = Vec::new();
        if let Some(b) = self.online {
            q.push(("online", b.to_string()));
        }
        let resp = self
            .client
            .http()
            .get(self.client.url("/api/agents"))
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
            ListEnvelope::Wrapped { agents } => agents,
            ListEnvelope::Bare(v) => v,
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ListEnvelope {
    Wrapped { agents: Vec<Agent> },
    Bare(Vec<Agent>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SingleEnvelope {
    Wrapped { agent: Agent },
    Bare(Agent),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NamesEnvelope {
    Wrapped { names: Vec<String> },
    Bare(Vec<String>),
}
