//! Integration tests for the bus API, including the SSE stream.

use acc_client::{model::BusSendRequest, Client};
use futures_util::StreamExt;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client_for(server: &MockServer) -> Client {
    Client::new(server.uri(), "t").unwrap()
}

#[tokio::test]
async fn bus_send_uses_type_field_on_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/bus/send"))
        .and(body_partial_json(json!({"type": "hello", "from": "tester"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    client
        .bus()
        .send(&BusSendRequest {
            kind: "hello".into(),
            from: Some("tester".into()),
            ..Default::default()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn bus_messages_maps_type_field_to_kind() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/bus/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [
                {"id": "m-1", "seq": 1, "type": "tasks:claimed", "from": "a",
                 "ts": "2026-04-23T00:00:00Z",
                 "data": {"task_id": "t-1"}}
            ]
        })))
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let msgs = client.bus().messages(None, None).await.unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].kind.as_deref(), Some("tasks:claimed"));
    assert_eq!(msgs[0].from.as_deref(), Some("a"));
}

#[tokio::test]
async fn bus_stream_yields_each_data_frame() {
    let server = MockServer::start().await;

    // Two complete SSE frames plus a comment keep-alive, emitted as one body.
    // The parser must separate them by blank lines and yield two BusMsg values.
    let body = concat!(
        ": keepalive\n\n",
        "data: {\"id\":\"m-1\",\"type\":\"first\",\"seq\":1}\n\n",
        "data: {\"id\":\"m-2\",\"type\":\"second\",\"seq\":2}\n\n",
    );

    Mock::given(method("GET"))
        .and(path("/api/bus/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let client = client_for(&server).await;
    let stream = client.bus().stream();
    tokio::pin!(stream);

    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.kind.as_deref(), Some("first"));
    assert_eq!(first.id.as_deref(), Some("m-1"));

    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second.kind.as_deref(), Some("second"));
    assert_eq!(second.seq, Some(2));

    // Stream should end cleanly when the mock closes.
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn bus_stream_yields_frames_split_across_chunk_boundaries() {
    // Multi-line data fields get joined by the parser before JSON-decoding.
    // This exercises the "data:" concatenation path.
    let server = MockServer::start().await;
    let body = "data: {\"type\":\"multi\",\ndata: \"id\":\"m-x\"}\n\n";
    Mock::given(method("GET"))
        .and(path("/api/bus/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let client = client_for(&server).await;
    let stream = client.bus().stream();
    tokio::pin!(stream);
    let msg = stream.next().await.unwrap().unwrap();
    assert_eq!(msg.kind.as_deref(), Some("multi"));
    assert_eq!(msg.id.as_deref(), Some("m-x"));
}
