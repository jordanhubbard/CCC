mod helpers;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn bus_messages_starts_empty() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    // Response may be an array or {"messages": [...]}
    let is_array = body.is_array();
    let has_messages = body.get("messages").is_some();
    assert!(is_array || has_messages, "bus/messages must return array or object with messages; got: {body}");
}

#[tokio::test]
async fn bus_send_broadcasts_message() {
    let srv = helpers::TestServer::new().await;

    let resp = helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from": "test-sender",
            "to": "all",
            "type": "ping",
            "subject": "test-ping",
            "body": {"msg": "hello from test"}
        })),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK, "bus/send should accept the message");
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], json!(true));
}

#[tokio::test]
async fn bus_send_message_appears_in_history() {
    let srv = helpers::TestServer::new().await;

    // Send a message
    helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from": "test-agent",
            "to": "all",
            "type": "test.probe",
            "subject": "test-subject-unique-12345",
        })),
    ).await;

    // Should appear in messages
    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    let messages = if body.is_array() {
        body.as_array().cloned().unwrap_or_default()
    } else {
        body["messages"].as_array().cloned().unwrap_or_default()
    };
    assert!(
        messages.iter().any(|m| m["subject"] == json!("test-subject-unique-12345")),
        "sent message should appear in history; got {} messages", messages.len()
    );
}

#[tokio::test]
async fn bus_presence_returns_agent_list() {
    let srv = helpers::TestServer::new().await;
    let resp = helpers::call(&srv.app, helpers::get("/api/bus/presence")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── blob metadata enrichment ─────────────────────────────────────────────────

/// A `type=blob` message with `mime=image/png` and `enc=base64` must have a
/// `blob_meta` object attached by /bus/messages that describes exactly how the
/// viewer should render it.
#[tokio::test]
async fn blob_message_gets_blob_meta_injected() {
    let srv = helpers::TestServer::new().await;

    // 1-pixel transparent PNG encoded as base64
    let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    // Send a blob message
    helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from": "test-agent",
            "to":   "all",
            "type": "blob",
            "mime": "image/png",
            "enc":  "base64",
            "subject": "test-image-blob",
            "body": png_b64,
        })),
    ).await;

    // Fetch messages and locate the blob
    let resp = helpers::call(
        &srv.app,
        helpers::get("/api/bus/messages?type=blob"),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    let msgs = body.as_array().expect("expected array");

    let blob_msg = msgs.iter()
        .find(|m| m["subject"] == json!("test-image-blob"))
        .expect("blob message should appear in /bus/messages");

    // blob_meta must be present
    let meta = blob_msg.get("blob_meta").expect("blob_meta must be injected by server");

    assert_eq!(meta["mime"],      json!("image/png"),  "mime mismatch");
    assert_eq!(meta["enc"],       json!("base64"),      "enc mismatch");
    assert_eq!(meta["render_as"], json!("image"),       "render_as must be 'image' for image/* MIME");

    let size: u64 = meta["size_bytes"].as_u64().expect("size_bytes must be a number");
    assert!(size > 0, "size_bytes must be non-zero");

    let blob_uri = meta["blob_uri"].as_str().expect("blob_uri must be a string");
    assert!(
        blob_uri.starts_with("data:image/png;base64,"),
        "blob_uri should be a base64 data-URI for inline blobs; got: {blob_uri}"
    );
}

/// A blob message with a pre-existing `blob_uri` field (written by the storage
/// layer) must have that URI preserved verbatim in `blob_meta.blob_uri`.
#[tokio::test]
async fn blob_meta_preserves_storage_layer_blob_uri() {
    let srv = helpers::TestServer::new().await;

    let storage_uri = "/api/fs/read?path=blobs/2026/04/test-audio.ogg";

    helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from":     "storage-agent",
            "to":       "all",
            "type":     "blob",
            "mime":     "audio/ogg",
            "enc":      "none",
            "subject":  "test-audio-blob",
            "body":     "",
            "blob_uri": storage_uri,
        })),
    ).await;

    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages?type=blob")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let msgs = helpers::body_json(resp).await;
    let msgs = msgs.as_array().unwrap();

    let msg = msgs.iter()
        .find(|m| m["subject"] == json!("test-audio-blob"))
        .expect("audio blob must appear in history");

    let meta = msg.get("blob_meta").expect("blob_meta must be present");
    assert_eq!(meta["render_as"], json!("audio"), "audio/* must render as audio");
    assert_eq!(
        meta["blob_uri"].as_str().unwrap(),
        storage_uri,
        "pre-existing blob_uri must be preserved verbatim"
    );
}

/// `render_as` must be `"video"` for `video/*` MIME types.
#[tokio::test]
async fn blob_meta_render_as_video_for_video_mime() {
    let srv = helpers::TestServer::new().await;

    helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from": "cam-agent",
            "to":   "all",
            "type": "blob",
            "mime": "video/mp4",
            "enc":  "base64",
            "subject": "test-video-blob",
            "body": "AAABIAAAA",   // stub base64, not real video
        })),
    ).await;

    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages?type=blob")).await;
    let msgs = helpers::body_json(resp).await;
    let msg = msgs.as_array().unwrap()
        .iter().find(|m| m["subject"] == json!("test-video-blob"))
        .expect("video blob must appear");

    assert_eq!(msg["blob_meta"]["render_as"], json!("video"));
    let uri = msg["blob_meta"]["blob_uri"].as_str().unwrap();
    assert!(uri.starts_with("data:video/mp4;base64,"), "video data-URI expected; got: {uri}");
}

/// Unknown / binary MIME types must fall back to `render_as = "download"`.
#[tokio::test]
async fn blob_meta_render_as_download_for_unknown_mime() {
    let srv = helpers::TestServer::new().await;

    helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from": "file-agent",
            "to":   "all",
            "type": "blob",
            "mime": "application/zip",
            "enc":  "base64",
            "subject": "test-zip-blob",
            "body": "UEsDBBQAAAAI",   // stub base64
        })),
    ).await;

    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages?type=blob")).await;
    let msgs = helpers::body_json(resp).await;
    let msg = msgs.as_array().unwrap()
        .iter().find(|m| m["subject"] == json!("test-zip-blob"))
        .expect("zip blob must appear");

    assert_eq!(msg["blob_meta"]["render_as"], json!("download"),
        "non-image/audio/video MIME must fall back to 'download'");
}

/// Non-blob messages (type=text, type=ping, etc.) must NOT have `blob_meta`.
#[tokio::test]
async fn non_blob_messages_do_not_get_blob_meta() {
    let srv = helpers::TestServer::new().await;

    helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from": "test-agent",
            "to":   "all",
            "type": "text",
            "mime": "text/plain",
            "subject": "test-text-no-blob-meta",
            "body": "just a regular message",
        })),
    ).await;

    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages")).await;
    let msgs = helpers::body_json(resp).await;
    let msg = msgs.as_array().unwrap()
        .iter().find(|m| m["subject"] == json!("test-text-no-blob-meta"))
        .expect("text message must appear");

    assert!(
        msg.get("blob_meta").is_none(),
        "blob_meta must NOT be present on non-blob messages"
    );
}

/// Blob message without a `mime` field defaults to `application/octet-stream`
/// and `render_as = "download"`.
#[tokio::test]
async fn blob_meta_defaults_when_mime_absent() {
    let srv = helpers::TestServer::new().await;

    helpers::call(
        &srv.app,
        helpers::post_json("/api/bus/send", &json!({
            "from": "bare-agent",
            "to":   "all",
            "type": "blob",
            "subject": "test-bare-blob",
            "body": "c29tZSByYW5kb20gYnl0ZXM=",   // base64
        })),
    ).await;

    let resp = helpers::call(&srv.app, helpers::get("/api/bus/messages?type=blob")).await;
    let msgs = helpers::body_json(resp).await;
    let msg = msgs.as_array().unwrap()
        .iter().find(|m| m["subject"] == json!("test-bare-blob"))
        .expect("bare blob must appear");

    let meta = msg.get("blob_meta").expect("blob_meta must be present even without mime");
    assert_eq!(meta["mime"],      json!("application/octet-stream"));
    assert_eq!(meta["render_as"], json!("download"));
}

// ── viewer endpoint ───────────────────────────────────────────────────────────

/// GET /bus/viewer must return HTTP 200 with Content-Type: text/html.
#[tokio::test]
async fn bus_viewer_returns_html() {
    use axum::http::Request;
    use axum::body::Body;

    let srv = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/bus/viewer")
        .body(Body::empty())
        .unwrap();

    let resp = helpers::call(&srv.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK, "/bus/viewer must return 200");

    let ct = resp.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "content-type must be text/html; got: {ct}");

    let body = helpers::body_bytes(resp).await;
    let html = std::str::from_utf8(&body).expect("HTML must be valid UTF-8");

    // Must be a complete HTML document
    assert!(html.contains("<!DOCTYPE html>"),   "must have DOCTYPE");
    assert!(html.contains("AgentBus Viewer"),   "must contain viewer title");

    // All four SPEC.md rendering hints must be present in the JS
    assert!(html.contains("renderAs === 'image'"),    "must handle image/* rendering");
    assert!(html.contains("renderAs === 'audio'"),    "must handle audio/* rendering");
    assert!(html.contains("renderAs === 'video'"),    "must handle video/* rendering");
    assert!(html.contains("blob-download"),           "must have download fallback for other types");

    // The viewer must use blob_meta.blob_uri as the src
    assert!(html.contains("blob_meta"),  "must reference blob_meta from API");
    assert!(html.contains("blob_uri"),   "must use blob_uri as src");

    // SSE + REST hooks
    assert!(html.contains("/bus/stream"),    "must connect to SSE stream");
    assert!(html.contains("/bus/messages"),  "must load history from /bus/messages");
    assert!(html.contains("/bus/presence"),  "must poll /bus/presence");
}

/// GET /api/bus/viewer (the /api/ alias) must also return 200 HTML.
#[tokio::test]
async fn api_bus_viewer_alias_returns_html() {
    use axum::http::Request;
    use axum::body::Body;

    let srv = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/bus/viewer")
        .body(Body::empty())
        .unwrap();

    let resp = helpers::call(&srv.app, req).await;
    assert_eq!(resp.status(), StatusCode::OK, "/api/bus/viewer alias must return 200");
    let ct = resp.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/html"), "content-type must be text/html");
}

/// The viewer HTML must contain the <audio controls> and <video controls>
/// patterns specified in SPEC.md.
#[tokio::test]
async fn bus_viewer_html_has_audio_and_video_controls() {
    use axum::http::Request;
    use axum::body::Body;

    let srv = helpers::TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/bus/viewer")
        .body(Body::empty())
        .unwrap();

    let resp = helpers::call(&srv.app, req).await;
    let body = helpers::body_bytes(resp).await;
    let html = std::str::from_utf8(&body).unwrap();

    // The JS must construct <audio controls> and <video controls> elements
    assert!(html.contains("controls"),
        "viewer must use 'controls' attribute for audio/video elements per SPEC.md");
    assert!(html.contains("<audio"),
        "viewer must build <audio> element for audio/* MIME");
    assert!(html.contains("<video"),
        "viewer must build <video> element for video/* MIME");
    assert!(html.contains("<img"),
        "viewer must build <img> element for image/* MIME");
}

