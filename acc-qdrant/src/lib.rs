pub mod client;
pub mod embed;
pub mod error;
pub mod types;
pub mod utils;

pub use client::QdrantClient;
pub use embed::EmbedClient;
pub use error::QdrantError;
pub use types::{Point, SearchResult};
pub use utils::{chunk_text, deterministic_id};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── collection_exists ─────────────────────────────────────────────────

    #[tokio::test]
    async fn collection_exists_returns_false_for_404() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/collections/nonexistent"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "status": { "error": "Not found: Collection `nonexistent` doesn't exist!" },
                "time": 0.0
            })))
            .mount(&server)
            .await;

        let client = QdrantClient::new(&server.uri(), None).unwrap();
        let exists = client.collection_exists("nonexistent").await.unwrap();
        assert!(!exists, "expected false for 404 response");
    }

    #[tokio::test]
    async fn collection_exists_returns_true_for_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/collections/my-col"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": { "status": "green" },
                "status": "ok",
                "time": 0.001
            })))
            .mount(&server)
            .await;

        let client = QdrantClient::new(&server.uri(), None).unwrap();
        let exists = client.collection_exists("my-col").await.unwrap();
        assert!(exists, "expected true for 200 response");
    }

    // ── upsert_points ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn upsert_points_sends_correct_json_body() {
        use wiremock::matchers::body_json;

        let server = MockServer::start().await;

        let expected_body = json!({
            "points": [
                {
                    "id": "aaaaaaaa-0000-0000-0000-000000000001",
                    "vector": [0.1_f32, 0.2_f32, 0.3_f32],
                    "payload": { "text": "hello" }
                }
            ]
        });

        Mock::given(method("PUT"))
            .and(path("/collections/test-col/points"))
            .and(body_json(expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": { "operation_id": 1, "status": "completed" },
                "status": "ok",
                "time": 0.001
            })))
            .mount(&server)
            .await;

        let client = QdrantClient::new(&server.uri(), None).unwrap();
        let points = vec![Point {
            id: "aaaaaaaa-0000-0000-0000-000000000001".to_owned(),
            vector: vec![0.1, 0.2, 0.3],
            payload: json!({ "text": "hello" }),
        }];
        client.upsert_points("test-col", points).await.unwrap();
    }

    // ── EmbedClient::embed ────────────────────────────────────────────────

    #[tokio::test]
    async fn embed_client_parses_response_correctly() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "embedding": [0.1_f32, 0.2_f32, 0.3_f32], "index": 0, "object": "embedding" },
                    { "embedding": [0.4_f32, 0.5_f32, 0.6_f32], "index": 1, "object": "embedding" }
                ],
                "model": "text-embedding-3-large",
                "object": "list"
            })))
            .mount(&server)
            .await;

        let client = EmbedClient::new(&server.uri(), "test-key", "text-embedding-3-large").unwrap();
        let embeddings = client.embed(&["foo", "bar"]).await.unwrap();

        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0], vec![0.1_f32, 0.2_f32, 0.3_f32]);
        assert_eq!(embeddings[1], vec![0.4_f32, 0.5_f32, 0.6_f32]);
    }
}
