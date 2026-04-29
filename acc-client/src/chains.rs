//! Conversation-chain operations on `/api/chains`.

use crate::{Client, Result};
use serde_json::Value;

pub struct ChainsApi<'a> {
    pub(crate) client: &'a Client,
}

impl<'a> ChainsApi<'a> {
    /// Create or update a chain. Returns the full chain envelope.
    pub async fn upsert(&self, chain: &Value) -> Result<Value> {
        self.client
            .request_json("POST", "/api/chains", Some(chain))
            .await
    }

    /// Append one immutable event to a chain. The server idempotently ignores
    /// duplicate `source_event_id` values within the same chain.
    pub async fn append_event(&self, chain_id: &str, event: &Value) -> Result<Value> {
        let path = format!("/api/chains/{}/events", urlencoding(chain_id));
        self.client.request_json("POST", &path, Some(event)).await
    }

    /// Link a task back to the chain that requested or discussed it.
    pub async fn link_task(
        &self,
        chain_id: &str,
        task_id: &str,
        relationship: &str,
    ) -> Result<Value> {
        let path = format!("/api/chains/{}/tasks", urlencoding(chain_id));
        let body = serde_json::json!({
            "task_id": task_id,
            "relationship": relationship,
        });
        self.client.request_json("POST", &path, Some(&body)).await
    }

    /// Fetch a full chain, including events, participants, entities, and tasks.
    pub async fn get(&self, chain_id: &str) -> Result<Value> {
        let path = format!("/api/chains/{}", urlencoding(chain_id));
        self.client.request_json("GET", &path, None).await
    }
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '/' => vec!['%', '2', 'F'],
            ':' => vec!['%', '3', 'A'],
            ' ' => vec!['%', '2', '0'],
            c => vec![c],
        })
        .collect()
}
