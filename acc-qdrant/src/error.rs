use thiserror::Error;

#[derive(Debug, Error)]
pub enum QdrantError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Qdrant API error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Unexpected response shape: {0}")]
    Parse(String),

    #[error("Configuration error: {0}")]
    Config(String),
}
