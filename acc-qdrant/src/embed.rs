use serde::Deserialize;
use serde_json::json;
use tracing::debug;

use crate::error::QdrantError;

/// Client for an OpenAI-compatible embeddings endpoint.
///
/// Works with NVIDIA NIM (`text-embedding-3-large`) and any other provider
/// that implements the `/v1/embeddings` endpoint.
pub struct EmbedClient {
    base_url: String,
    api_key: String,
    model: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

impl EmbedClient {
    /// Create a new embedding client.
    ///
    /// * `base_url` — root URL of the OpenAI-compatible API, e.g.
    ///   `"https://inference-api.nvidia.com/v1"`.
    /// * `api_key`  — bearer token / API key.
    /// * `model`    — model name as accepted by the endpoint, e.g.
    ///   `"text-embedding-3-large"`.
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Result<Self, QdrantError> {
        if api_key.is_empty() {
            return Err(QdrantError::Config("api_key must not be empty".to_owned()));
        }
        let http = reqwest::Client::builder()
            .build()
            .map_err(QdrantError::Http)?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            http,
        })
    }

    /// Embed a batch of texts and return one vector per input string.
    pub async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, QdrantError> {
        debug!(
            "embedding {} texts with model '{}'",
            texts.len(),
            self.model
        );
        let body = json!({
            "model": self.model,
            "input": texts
        });
        let resp = self
            .http
            .post(format!("{}/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(QdrantError::Http)?;

        let status = resp.status();
        if !status.is_success() {
            let message = resp
                .text()
                .await
                .unwrap_or_else(|_| "(failed to read body)".to_owned());
            return Err(QdrantError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let embed_resp: EmbedResponse = resp.json().await.map_err(QdrantError::Http)?;
        Ok(embed_resp.data.into_iter().map(|d| d.embedding).collect())
    }
}
