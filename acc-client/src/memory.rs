//! Memory endpoints: semantic search and store.

use crate::{Client, Error, Result};
use acc_model::{MemoryHit, MemorySearchRequest, MemoryStoreRequest};
use serde::Deserialize;

#[derive(Debug, Clone, Copy)]
pub struct MemoryApi<'a> {
    pub(crate) client: &'a Client,
}

impl<'a> MemoryApi<'a> {
    /// POST /api/memory/search
    pub async fn search(self, req: &MemorySearchRequest) -> Result<Vec<MemoryHit>> {
        let resp = self
            .client
            .http()
            .post(self.client.url("/api/memory/search"))
            .json(req)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(Error::from_response(status, &bytes));
        }
        let env: SearchEnvelope = serde_json::from_slice(&bytes)?;
        Ok(match env {
            SearchEnvelope::Wrapped { results } => results,
            SearchEnvelope::Hits { hits } => hits,
            SearchEnvelope::Bare(v) => v,
        })
    }

    /// POST /api/memory/store
    pub async fn store(self, req: &MemoryStoreRequest) -> Result<()> {
        let resp = self
            .client
            .http()
            .post(self.client.url("/api/memory/store"))
            .json(req)
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

#[derive(Deserialize)]
#[serde(untagged)]
enum SearchEnvelope {
    Wrapped { results: Vec<MemoryHit> },
    Hits { hits: Vec<MemoryHit> },
    Bare(Vec<MemoryHit>),
}
