use serde_json::{json, Value};
use tracing::debug;

use crate::{
    error::QdrantError,
    types::{Point, QdrantPoint, QdrantScrollPoint, QdrantSearchHit, SearchResult},
};

/// Async HTTP client for the Qdrant REST API.
///
/// Does not depend on any Qdrant SDK — talks directly to the REST endpoints
/// documented at <https://qdrant.github.io/qdrant/redoc/>.
pub struct QdrantClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl QdrantClient {
    /// Create a new client.
    ///
    /// `base_url` should be the root URL of the Qdrant instance, e.g.
    /// `"http://localhost:6333"`.  No trailing slash required.
    pub fn new(base_url: &str, api_key: Option<&str>) -> Result<Self, QdrantError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(QdrantError::Http)?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.map(str::to_owned),
            http,
        })
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let req = self.http.get(&url);
        self.with_auth(req)
    }

    fn put(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let req = self.http.put(&url);
        self.with_auth(req)
    }

    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let req = self.http.post(&url);
        self.with_auth(req)
    }

    #[allow(dead_code)]
    fn delete(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let req = self.http.delete(&url);
        self.with_auth(req)
    }

    fn with_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            req.header("api-key", key)
        } else {
            req
        }
    }

    async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, QdrantError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let message = resp
            .text()
            .await
            .unwrap_or_else(|_| "(failed to read body)".to_owned());
        Err(QdrantError::Api {
            status: status.as_u16(),
            message,
        })
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Returns `true` if the collection exists, `false` on a 404.
    pub async fn collection_exists(&self, name: &str) -> Result<bool, QdrantError> {
        let resp = self
            .get(&format!("/collections/{name}"))
            .send()
            .await
            .map_err(QdrantError::Http)?;

        if resp.status().as_u16() == 404 {
            return Ok(false);
        }
        Self::check_status(resp).await?;
        Ok(true)
    }

    /// Create a new collection with a fixed vector dimension.
    ///
    /// Uses cosine distance — suitable for normalised embeddings.
    pub async fn create_collection(&self, name: &str, vector_size: u64) -> Result<(), QdrantError> {
        debug!("creating collection '{name}' with vector_size={vector_size}");
        let body = json!({
            "vectors": {
                "size": vector_size,
                "distance": "Cosine"
            }
        });
        let resp = self
            .put(&format!("/collections/{name}"))
            .json(&body)
            .send()
            .await
            .map_err(QdrantError::Http)?;
        Self::check_status(resp).await?;
        Ok(())
    }

    /// Upsert a batch of points into `collection`.
    pub async fn upsert_points(
        &self,
        collection: &str,
        points: Vec<Point>,
    ) -> Result<(), QdrantError> {
        debug!("upserting {} points into '{collection}'", points.len());
        let qdrant_points: Vec<QdrantPoint> = points.into_iter().map(Into::into).collect();
        let body = json!({ "points": qdrant_points });
        let resp = self
            .put(&format!("/collections/{collection}/points"))
            .json(&body)
            .send()
            .await
            .map_err(QdrantError::Http)?;
        Self::check_status(resp).await?;
        Ok(())
    }

    /// Perform a nearest-neighbour vector search.
    ///
    /// `filter` is an optional Qdrant filter object (see Qdrant filter docs).
    pub async fn search_points(
        &self,
        collection: &str,
        vector: &[f32],
        limit: u64,
        filter: Option<Value>,
    ) -> Result<Vec<SearchResult>, QdrantError> {
        debug!("searching '{collection}' top-{limit}");
        let mut body = json!({
            "vector": vector,
            "limit": limit,
            "with_payload": true
        });
        if let Some(f) = filter {
            body["filter"] = f;
        }
        let resp = self
            .post(&format!("/collections/{collection}/points/search"))
            .json(&body)
            .send()
            .await
            .map_err(QdrantError::Http)?;
        let resp = Self::check_status(resp).await?;
        let raw: Value = resp.json().await.map_err(QdrantError::Http)?;
        let hits: Vec<QdrantSearchHit> = serde_json::from_value(raw["result"].clone())
            .map_err(|e| QdrantError::Parse(format!("search result: {e}")))?;

        hits.into_iter().map(SearchResult::try_from).collect()
    }

    /// Delete points by their string IDs.
    pub async fn delete_points(&self, collection: &str, ids: &[String]) -> Result<(), QdrantError> {
        debug!("deleting {} points from '{collection}'", ids.len());
        let body = json!({ "points": ids });
        let resp = self
            .post(&format!("/collections/{collection}/points/delete"))
            .json(&body)
            .send()
            .await
            .map_err(QdrantError::Http)?;
        Self::check_status(resp).await?;
        Ok(())
    }

    /// Scroll through all points in a collection (up to `limit` results).
    pub async fn scroll_all(
        &self,
        collection: &str,
        limit: u32,
    ) -> Result<Vec<SearchResult>, QdrantError> {
        debug!("scrolling '{collection}' limit={limit}");
        let body = json!({
            "limit": limit,
            "with_payload": true
        });
        let resp = self
            .post(&format!("/collections/{collection}/points/scroll"))
            .json(&body)
            .send()
            .await
            .map_err(QdrantError::Http)?;
        let resp = Self::check_status(resp).await?;
        let raw: Value = resp.json().await.map_err(QdrantError::Http)?;
        let points: Vec<QdrantScrollPoint> =
            serde_json::from_value(raw["result"]["points"].clone())
                .map_err(|e| QdrantError::Parse(format!("scroll result: {e}")))?;

        points.into_iter().map(SearchResult::try_from).collect()
    }

    /// Upsert raw JSON point objects (supports both UUID string and u64 integer IDs).
    ///
    /// Each element in `points` must be a JSON object with `id`, `vector`, and `payload`.
    pub async fn upsert_points_raw(
        &self,
        collection: &str,
        points: Vec<serde_json::Value>,
    ) -> Result<(), QdrantError> {
        debug!("upserting {} raw points into '{collection}'", points.len());
        let body = json!({ "points": points });
        let resp = self
            .put(&format!("/collections/{collection}/points"))
            .json(&body)
            .send()
            .await
            .map_err(QdrantError::Http)?;
        Self::check_status(resp).await?;
        Ok(())
    }

    /// Return the number of points currently in `collection`, or 0 on error/404.
    pub async fn collection_point_count(&self, name: &str) -> u64 {
        let resp = match self.get(&format!("/collections/{name}")).send().await {
            Ok(r) => r,
            Err(_) => return 0,
        };
        if !resp.status().is_success() {
            return 0;
        }
        let raw: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return 0,
        };
        raw["result"]["points_count"].as_u64().unwrap_or(0)
    }

    /// Ensure a collection exists, creating it (with keyword payload indexes) if absent.
    ///
    /// Returns the current point count (0 for newly-created collections).
    pub async fn ensure_collection(
        &self,
        name: &str,
        vector_size: u64,
        index_fields: &[&str],
    ) -> Result<u64, QdrantError> {
        if !self.collection_exists(name).await? {
            self.create_collection(name, vector_size).await?;
            for field in index_fields {
                let body = json!({
                    "field_name": field,
                    "field_schema": "keyword"
                });
                // Ignore index-creation errors (index may already exist on retry)
                let _ = self
                    .put(&format!("/collections/{name}/index"))
                    .json(&body)
                    .send()
                    .await;
            }
            Ok(0)
        } else {
            Ok(self.collection_point_count(name).await)
        }
    }
}
