use serde::{Deserialize, Serialize};

/// A single vector point to be stored in Qdrant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    /// UUID string used as the point ID.
    pub id: String,
    pub vector: Vec<f32>,
    pub payload: serde_json::Value,
}

/// A result returned by a vector similarity search.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub payload: serde_json::Value,
}

// ── Internal Qdrant REST shapes ───────────────────────────────────────────

/// Point as accepted by the Qdrant upsert endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct QdrantPoint {
    pub id: String,
    pub vector: Vec<f32>,
    pub payload: serde_json::Value,
}

impl From<Point> for QdrantPoint {
    fn from(p: Point) -> Self {
        QdrantPoint {
            id: p.id,
            vector: p.vector,
            payload: p.payload,
        }
    }
}

/// Response shape for a search hit from Qdrant.
#[derive(Debug, Deserialize)]
pub(crate) struct QdrantSearchHit {
    pub id: serde_json::Value,
    pub score: f32,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl TryFrom<QdrantSearchHit> for SearchResult {
    type Error = crate::error::QdrantError;

    fn try_from(hit: QdrantSearchHit) -> Result<Self, Self::Error> {
        let id = match &hit.id {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            other => {
                return Err(crate::error::QdrantError::Parse(format!(
                    "unexpected id type: {other}"
                )))
            }
        };
        Ok(SearchResult {
            id,
            score: hit.score,
            payload: hit.payload,
        })
    }
}

/// Response shape for a scroll hit from Qdrant.
#[derive(Debug, Deserialize)]
pub(crate) struct QdrantScrollPoint {
    pub id: serde_json::Value,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl TryFrom<QdrantScrollPoint> for SearchResult {
    type Error = crate::error::QdrantError;

    fn try_from(p: QdrantScrollPoint) -> Result<Self, Self::Error> {
        let id = match &p.id {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            other => {
                return Err(crate::error::QdrantError::Parse(format!(
                    "unexpected id type: {other}"
                )))
            }
        };
        Ok(SearchResult {
            id,
            score: 0.0,
            payload: p.payload,
        })
    }
}
