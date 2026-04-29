mod helpers;

use acc_server::routes::blobs::{b64_decode, b64_encode};
use helpers::{body_json, call, delete, get, post_json, TestServer};
use serde_json::{json, Value};

// ── helpers ───────────────────────────────────────────────────────────────────

async fn upload(srv: &TestServer, mime: &str, data: &[u8], binary: bool) -> (u16, Value) {
    let body = json!({
        "mime_type": mime,
        "enc":       if binary { "base64" } else { "none" },
        "data":      if binary { b64_encode(data) } else { String::from_utf8_lossy(data).into_owned() },
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let status = resp.status().as_u16();
    (status, body_json(resp).await)
}

async fn download(srv: &TestServer, blob_id: &str) -> (u16, Value) {
    let resp = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    let status = resp.status().as_u16();
    (status, body_json(resp).await)
}

// Upload, then download and verify the round-trip.
async fn round_trip(srv: &TestServer, mime: &str, data: &[u8], binary: bool) {
    let (status, body) = upload(srv, mime, data, binary).await;
    assert_eq!(status, 200, "upload failed for {mime}: {body}");
    let blob_id = body["blob_id"].as_str().unwrap().to_string();
    assert_eq!(body["complete"], json!(true));

    let (dl_status, dl) = download(srv, &blob_id).await;
    assert_eq!(dl_status, 200, "download failed for {mime}: {dl}");
    assert_eq!(dl["mime_type"].as_str().unwrap(), mime);
    assert_eq!(dl["enc"].as_str().unwrap(), "base64");

    let returned = b64_decode(dl["data"].as_str().unwrap()).expect("valid base64 in response");
    assert_eq!(returned, data, "data mismatch for mime={mime}");
}

// ── Upload / download round-trips for all 25 media types ─────────────────────

#[tokio::test]
async fn test_text_plain_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "text/plain", b"hello world", false).await;
}

#[tokio::test]
async fn test_text_markdown_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "text/markdown", b"# Heading\nParagraph", false).await;
}

#[tokio::test]
async fn test_text_html_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "text/html", b"<h1>Hi</h1>", false).await;
}

#[tokio::test]
async fn test_application_json_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "application/json", b"{\"key\":\"value\"}", false).await;
}

#[tokio::test]
async fn test_image_svg_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "image/svg+xml", b"<svg><rect/></svg>", false).await;
}

#[tokio::test]
async fn test_audio_wav_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "audio/wav", b"\x52\x49\x46\x46\x00\x00", true).await;
}

#[tokio::test]
async fn test_audio_mp3_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "audio/mp3", b"\xff\xfb\x90\x00", true).await;
}

#[tokio::test]
async fn test_audio_ogg_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "audio/ogg", b"OggS\x00\x02", true).await;
}

#[tokio::test]
async fn test_audio_flac_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "audio/flac", b"fLaC\x00", true).await;
}

#[tokio::test]
async fn test_video_mp4_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "video/mp4", b"\x00\x00\x00\x1cftyp", true).await;
}

#[tokio::test]
async fn test_video_webm_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "video/webm", b"\x1a\x45\xdf\xa3", true).await;
}

#[tokio::test]
async fn test_video_ogg_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "video/ogg", b"OggS\x00\x02video", true).await;
}

#[tokio::test]
async fn test_image_png_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "image/png", b"\x89PNG\r\n\x1a\n", true).await;
}

#[tokio::test]
async fn test_image_jpeg_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "image/jpeg", b"\xff\xd8\xff\xe0", true).await;
}

#[tokio::test]
async fn test_image_gif_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "image/gif", b"GIF89a\x01\x00", true).await;
}

#[tokio::test]
async fn test_image_webp_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "image/webp", b"RIFF\x00\x00\x00\x00WEBP", true).await;
}

#[tokio::test]
async fn test_application_octet_stream_round_trip() {
    let srv = TestServer::new().await;
    round_trip(
        &srv,
        "application/octet-stream",
        b"\x00\x01\x02\x03\xff",
        true,
    )
    .await;
}

// ── Upload / download round-trips for 3-D model types ────────────────────────

#[tokio::test]
async fn test_model_gltf_json_round_trip() {
    let srv = TestServer::new().await;
    round_trip(
        &srv,
        "model/gltf+json",
        br#"{"asset":{"version":"2.0"}}"#,
        false,
    )
    .await;
}

#[tokio::test]
async fn test_model_gltf_binary_round_trip() {
    let srv = TestServer::new().await;
    // GLB magic: 0x46546C67 ("glTF") little-endian
    round_trip(&srv, "model/gltf-binary", b"glTF\x02\x00\x00\x00", true).await;
}

#[tokio::test]
async fn test_model_obj_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "model/obj", b"# Wavefront OBJ\nv 0 0 0\n", false).await;
}

#[tokio::test]
async fn test_model_usdz_round_trip() {
    let srv = TestServer::new().await;
    // USDZ is a ZIP container; use a minimal ZIP local-file header magic.
    round_trip(&srv, "model/vnd.usdz+zip", b"PK\x03\x04", true).await;
}

#[tokio::test]
async fn test_model_stl_round_trip() {
    let srv = TestServer::new().await;
    // Binary STL: 80-byte header + uint32 triangle count (0).
    let mut stl = vec![0u8; 80];
    stl.extend_from_slice(&[0u8; 4]); // 0 triangles
    round_trip(&srv, "model/stl", &stl, true).await;
}

#[tokio::test]
async fn test_model_ply_round_trip() {
    let srv = TestServer::new().await;
    round_trip(
        &srv,
        "model/ply",
        b"ply\nformat binary_little_endian 1.0\nend_header\n",
        true,
    )
    .await;
}

#[tokio::test]
async fn test_model_vrml_round_trip() {
    let srv = TestServer::new().await;
    round_trip(&srv, "model/vrml", b"#VRML V2.0 utf8\n", false).await;
}

#[tokio::test]
async fn test_model_fbx_round_trip() {
    let srv = TestServer::new().await;
    // FBX binary magic: "Kaydara FBX Binary  \x00"
    round_trip(&srv, "model/fbx", b"Kaydara FBX Binary  \x00", true).await;
}

// ── Validation tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn test_binary_type_without_base64_enc_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type": "image/png",
        "enc":       "none",
        "data":      "aGVsbG8=",
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
    let j = body_json(resp).await;
    assert_eq!(
        j["error"].as_str().unwrap(),
        "binary_type_requires_base64_enc"
    );
}

#[tokio::test]
async fn test_unknown_mime_type_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type": "application/x-custom-unknown",
        "enc":       "base64",
        "data":      b64_encode(b"data"),
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
    let j = body_json(resp).await;
    assert_eq!(j["error"].as_str().unwrap(), "unknown_media_type");
}

#[tokio::test]
async fn test_upload_missing_mime_type_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "enc":  "base64",
        "data": "aGVsbG8=",
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
}

#[tokio::test]
async fn test_upload_invalid_base64_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type": "image/png",
        "enc":       "base64",
        "data":      "NOT!VALID!BASE64!!!",
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
    let j = body_json(resp).await;
    assert_eq!(j["error"].as_str().unwrap(), "invalid_base64");
}

// ── Blob metadata and listing ─────────────────────────────────────────────────

#[tokio::test]
async fn test_blob_list_returns_uploaded_blobs() {
    let srv = TestServer::new().await;
    let (_, b1) = upload(&srv, "text/plain", b"one", false).await;
    let (_, b2) = upload(&srv, "text/markdown", b"two", false).await;
    let id1 = b1["blob_id"].as_str().unwrap().to_string();
    let id2 = b2["blob_id"].as_str().unwrap().to_string();

    let resp = call(&srv.app, get("/api/bus/blobs")).await;
    assert_eq!(resp.status().as_u16(), 200);
    let list = body_json(resp).await;
    let blobs = list["blobs"].as_array().unwrap();
    let ids: Vec<&str> = blobs.iter().filter_map(|b| b["id"].as_str()).collect();
    assert!(ids.contains(&id1.as_str()));
    assert!(ids.contains(&id2.as_str()));
}

#[tokio::test]
async fn test_blob_meta_returns_correct_fields() {
    let srv = TestServer::new().await;
    let (_, body) = upload(&srv, "image/png", b"\x89PNG", true).await;
    let blob_id = body["blob_id"].as_str().unwrap().to_string();

    let resp = call(&srv.app, get(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(resp.status().as_u16(), 200);
    let meta = body_json(resp).await;
    assert_eq!(meta["id"].as_str().unwrap(), blob_id);
    assert_eq!(meta["mime_type"].as_str().unwrap(), "image/png");
    assert_eq!(meta["complete"], json!(true));
    assert_eq!(meta["total_chunks"], json!(1));
    assert_eq!(meta["chunks_received"], json!(1));
}

#[tokio::test]
async fn test_blob_meta_unknown_id_returns_404() {
    let srv = TestServer::new().await;
    let resp = call(&srv.app, get("/api/bus/blobs/does-not-exist")).await;
    assert_eq!(resp.status().as_u16(), 404);
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_blob_delete_removes_blob() {
    let srv = TestServer::new().await;
    let (_, body) = upload(&srv, "text/plain", b"bye", false).await;
    let blob_id = body["blob_id"].as_str().unwrap().to_string();

    let del = call(&srv.app, delete(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(del.status().as_u16(), 200);

    // Meta and download both 404 now
    let meta = call(&srv.app, get(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(meta.status().as_u16(), 404);

    let dl = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    assert_eq!(dl.status().as_u16(), 404);
}

#[tokio::test]
async fn test_blob_delete_unknown_id_returns_404() {
    let srv = TestServer::new().await;
    let resp = call(&srv.app, delete("/api/bus/blobs/ghost-blob")).await;
    assert_eq!(resp.status().as_u16(), 404);
}

// ── TTL expiry ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_blob_expired_returns_410() {
    let srv = TestServer::new().await;
    // Upload with ttl=1, wait 2 seconds, download should return 410
    let body = json!({
        "mime_type":   "text/plain",
        "enc":         "none",
        "data":        "expires soon",
        "total_chunks": 1,
        "ttl_seconds": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let (status, dl) = download(&srv, &blob_id).await;
    assert_eq!(
        status, 410,
        "expected 410 Gone for expired blob, got {status}: {dl}"
    );
    assert_eq!(dl["error"].as_str().unwrap(), "blob_expired");
}

#[tokio::test]
async fn test_blob_no_ttl_does_not_expire() {
    let srv = TestServer::new().await;
    // ttl_seconds=0 → no expiry
    let body = json!({
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "lives forever",
        "total_chunks": 1,
        "ttl_seconds":  0,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    let (status, _) = download(&srv, &blob_id).await;
    assert_eq!(status, 200);
}

// ── Access control ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_blob_access_control_denies_unknown_agent() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "secret",
        "total_chunks":   1,
        "allowed_agents": ["natasha"],
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    // Download without ?agent= → 403
    let no_agent = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    assert_eq!(no_agent.status().as_u16(), 403);

    // Download with wrong agent → 403
    let wrong = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download?agent=boris", blob_id)),
    )
    .await;
    assert_eq!(wrong.status().as_u16(), 403);
}

#[tokio::test]
async fn test_blob_access_control_allows_listed_agent() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "for natasha only",
        "total_chunks":   1,
        "allowed_agents": ["natasha"],
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    let ok = call(
        &srv.app,
        get(&format!(
            "/api/bus/blobs/{}/download?agent=natasha",
            blob_id
        )),
    )
    .await;
    assert_eq!(ok.status().as_u16(), 200);
}

#[tokio::test]
async fn test_blob_empty_allowed_agents_is_public() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "public data",
        "total_chunks":   1,
        "allowed_agents": [],
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    // Any agent or no agent can download
    let ok = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    assert_eq!(ok.status().as_u16(), 200);
}

// ── Multi-chunk upload ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_multi_chunk_blob_upload() {
    let srv = TestServer::new().await;
    let data_part1 = b"chunk-one-";
    let data_part2 = b"chunk-two";
    let full_data: Vec<u8> = data_part1
        .iter()
        .chain(data_part2.iter())
        .copied()
        .collect();
    let blob_id = "multi-chunk-test-blob";

    // Chunk 0
    let body1 = json!({
        "blob_id":      blob_id,
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         String::from_utf8_lossy(data_part1),
        "chunk_index":  0,
        "total_chunks": 2,
    });
    let resp1 = call(&srv.app, post_json("/api/bus/blobs/upload", &body1)).await;
    let j1 = body_json(resp1).await;
    assert_eq!(j1["complete"], json!(false));

    // Chunk 1
    let body2 = json!({
        "blob_id":      blob_id,
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         String::from_utf8_lossy(data_part2),
        "chunk_index":  1,
        "total_chunks": 2,
    });
    let resp2 = call(&srv.app, post_json("/api/bus/blobs/upload", &body2)).await;
    let j2 = body_json(resp2).await;
    assert_eq!(j2["complete"], json!(true));
    assert_eq!(j2["chunks_received"], json!(2));

    let (status, dl) = download(&srv, blob_id).await;
    assert_eq!(status, 200);
    let returned = b64_decode(dl["data"].as_str().unwrap()).unwrap();
    assert_eq!(returned, full_data);
}

// ── bus.blob_ready event fires on completion ──────────────────────────────────

#[tokio::test]
async fn test_blob_ready_event_fired_on_completion() {
    let srv = TestServer::new().await;

    let body = json!({
        "mime_type":    "image/png",
        "enc":          "base64",
        "data":         b64_encode(b"\x89PNG"),
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    assert_eq!(j["complete"], json!(true));
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    // Give async log write a moment to flush.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let msgs_resp = call(&srv.app, get("/api/bus/messages")).await;
    assert_eq!(msgs_resp.status().as_u16(), 200);
    let msgs = body_json(msgs_resp).await;
    let arr = msgs.as_array().unwrap();
    let found = arr.iter().any(|m| {
        m["type"].as_str() == Some("bus.blob_ready") && m["blob_id"].as_str() == Some(&blob_id)
    });
    assert!(
        found,
        "bus.blob_ready event must appear in bus messages after upload"
    );
}

// ── DLQ ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_dlq_append_and_list() {
    let srv = TestServer::new().await;

    let body = json!({
        "error":   "unknown_type: blob/x-custom",
        "message": {"type": "blob/x-custom", "from": "boris", "data": "some payload"},
    });
    let resp = call(&srv.app, post_json("/api/bus/dlq", &body)).await;
    assert_eq!(resp.status().as_u16(), 200);
    let j = body_json(resp).await;
    assert_eq!(j["ok"], json!(true));
    let dlq_id = j["id"].as_str().unwrap().to_string();

    let list_resp = call(&srv.app, get("/api/bus/dlq")).await;
    assert_eq!(list_resp.status().as_u16(), 200);
    let list = body_json(list_resp).await;
    let entries = list["entries"].as_array().unwrap();
    let found = entries.iter().any(|e| e["id"].as_str() == Some(&dlq_id));
    assert!(found, "appended DLQ entry must appear in list");
}

#[tokio::test]
async fn test_dlq_redeliver_replays_to_bus() {
    let srv = TestServer::new().await;

    // Append
    let append_body = json!({
        "error":   "unhandled",
        "message": {"type": "custom.event", "from": "natasha"},
    });
    let ar = call(&srv.app, post_json("/api/bus/dlq", &append_body)).await;
    let dlq_id = body_json(ar).await["id"].as_str().unwrap().to_string();

    // Redeliver
    let redeliver_body = json!({"dlq_id": dlq_id});
    let rr = call(
        &srv.app,
        post_json("/api/bus/dlq/redeliver", &redeliver_body),
    )
    .await;
    assert_eq!(rr.status().as_u16(), 200);
    let rj = body_json(rr).await;
    assert_eq!(rj["ok"], json!(true));
    assert_eq!(rj["redelivered"], json!(true));

    // Redelivered message should appear in bus messages
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let msgs = body_json(call(&srv.app, get("/api/bus/messages")).await).await;
    let arr = msgs.as_array().unwrap();
    let found = arr.iter().any(|m| {
        m["dlq_redelivered"].as_bool() == Some(true) && m["dlq_id"].as_str() == Some(&dlq_id)
    });
    assert!(found, "redelivered message must appear in bus messages");
}

#[tokio::test]
async fn test_dlq_redeliver_missing_id_returns_404() {
    let srv = TestServer::new().await;
    let body = json!({"dlq_id": "dlq-does-not-exist"});
    let resp = call(&srv.app, post_json("/api/bus/dlq/redeliver", &body)).await;
    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test]
async fn test_dlq_redeliver_no_dlq_id_returns_422() {
    let srv = TestServer::new().await;
    let resp = call(&srv.app, post_json("/api/bus/dlq/redeliver", &json!({}))).await;
    assert_eq!(resp.status().as_u16(), 422);
}

// ── bus_send mime validation ──────────────────────────────────────────────────

#[tokio::test]
async fn test_bus_send_unknown_mime_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "type":    "media.share",
        "from":    "natasha",
        "mime":    "application/x-totally-unknown",
        "enc":     "base64",
        "payload": "aGVsbG8=",
    });
    let resp = call(&srv.app, post_json("/api/bus/send", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
    let j = body_json(resp).await;
    assert_eq!(j["error"].as_str().unwrap(), "unknown_media_type");
}

#[tokio::test]
async fn test_bus_send_binary_without_enc_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "type":    "media.share",
        "from":    "boris",
        "mime":    "image/png",
        // enc field omitted — binary type requires base64
        "payload": "aGVsbG8=",
    });
    let resp = call(&srv.app, post_json("/api/bus/send", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
    let j = body_json(resp).await;
    assert_eq!(
        j["error"].as_str().unwrap(),
        "binary_type_requires_base64_enc"
    );
}

#[tokio::test]
async fn test_bus_send_text_mime_accepted() {
    let srv = TestServer::new().await;
    let body = json!({
        "type":    "media.share",
        "from":    "natasha",
        "mime":    "text/plain",
        "payload": "hello",
    });
    let resp = call(&srv.app, post_json("/api/bus/send", &body)).await;
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn test_bus_send_binary_mime_with_base64_accepted() {
    let srv = TestServer::new().await;
    let body = json!({
        "type":    "media.share",
        "from":    "natasha",
        "mime":    "image/png",
        "enc":     "base64",
        "payload": b64_encode(b"\x89PNG\r\n"),
    });
    let resp = call(&srv.app, post_json("/api/bus/send", &body)).await;
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn test_bus_send_no_mime_field_accepted() {
    let srv = TestServer::new().await;
    // Existing messages without mime field must still work
    let body = json!({
        "type": "ping",
        "from": "natasha",
        "to":   "all",
    });
    let resp = call(&srv.app, post_json("/api/bus/send", &body)).await;
    assert_eq!(resp.status().as_u16(), 200);
}

// ── Auth gating ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_blob_upload_requires_auth() {
    use axum::body::Body;
    use axum::http::Request;
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "hi",
        "total_chunks": 1,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/bus/blobs/upload")
        .header("Content-Type", "application/json")
        // No Authorization header
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = call(&srv.app, req).await;
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn test_blob_download_requires_auth() {
    use axum::body::Body;
    use axum::http::Request;
    let srv = TestServer::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/bus/blobs/any-id/download")
        .body(Body::empty())
        .unwrap();
    let resp = call(&srv.app, req).await;
    assert_eq!(resp.status().as_u16(), 401);
}
