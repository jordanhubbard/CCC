mod helpers;

use acc_server::routes::blobs::{b64_decode, b64_encode};
use helpers::{body_json, call, delete, get, post_json, TestServer};
use serde_json::{json, Value};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Upload a single-chunk blob and return (status, body).
async fn upload_single(srv: &TestServer, mime: &str, data: &[u8], binary: bool) -> (u16, Value) {
    let body = json!({
        "mime_type":    mime,
        "enc":          if binary { "base64" } else { "none" },
        "data":         if binary { b64_encode(data) } else { String::from_utf8_lossy(data).into_owned() },
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let status = resp.status().as_u16();
    (status, body_json(resp).await)
}

/// Upload one numbered chunk of a multi-chunk blob and return (status, body).
async fn upload_chunk(
    srv: &TestServer,
    blob_id: &str,
    mime: &str,
    data: &[u8],
    binary: bool,
    chunk_index: u64,
    total_chunks: u64,
) -> (u16, Value) {
    let body = json!({
        "blob_id":      blob_id,
        "mime_type":    mime,
        "enc":          if binary { "base64" } else { "none" },
        "data":         if binary { b64_encode(data) } else { String::from_utf8_lossy(data).into_owned() },
        "chunk_index":  chunk_index,
        "total_chunks": total_chunks,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let status = resp.status().as_u16();
    (status, body_json(resp).await)
}

/// Download a blob and return (status, body).
async fn download(srv: &TestServer, blob_id: &str) -> (u16, Value) {
    let resp = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    let status = resp.status().as_u16();
    (status, body_json(resp).await)
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests 1–20: single-chunk baseline, validation, metadata, delete, TTL,
//             access-control, and bus-event prerequisites.
// ═════════════════════════════════════════════════════════════════════════════

// ── 1: text blob single-chunk round-trip ─────────────────────────────────────
#[tokio::test]
async fn test_01_single_chunk_text_upload_and_download() {
    let srv = TestServer::new().await;
    let data = b"single chunk text payload";
    let (status, body) = upload_single(&srv, "text/plain", data, false).await;
    assert_eq!(status, 200, "upload: {body}");
    assert_eq!(body["complete"], json!(true));
    assert_eq!(body["chunks_received"], json!(1));
    assert_eq!(body["total_chunks"], json!(1));

    let blob_id = body["blob_id"].as_str().unwrap().to_string();
    let (dl_status, dl) = download(&srv, &blob_id).await;
    assert_eq!(dl_status, 200, "download: {dl}");
    assert_eq!(dl["mime_type"].as_str().unwrap(), "text/plain");
    let returned = b64_decode(dl["data"].as_str().unwrap()).unwrap();
    assert_eq!(returned, data);
}

// ── 2: binary blob single-chunk round-trip ────────────────────────────────────
#[tokio::test]
async fn test_02_single_chunk_binary_upload_and_download() {
    let srv = TestServer::new().await;
    let data: &[u8] = b"\x89PNG\r\n\x1a\nfakeimagedata";
    let (status, body) = upload_single(&srv, "image/png", data, true).await;
    assert_eq!(status, 200, "upload: {body}");
    assert_eq!(body["complete"], json!(true));

    let blob_id = body["blob_id"].as_str().unwrap().to_string();
    let (dl_status, dl) = download(&srv, &blob_id).await;
    assert_eq!(dl_status, 200, "download: {dl}");
    let returned = b64_decode(dl["data"].as_str().unwrap()).unwrap();
    assert_eq!(returned, data);
}

// ── 3: upload response contains a stable blob_id ─────────────────────────────
#[tokio::test]
async fn test_03_upload_returns_blob_id() {
    let srv = TestServer::new().await;
    let (status, body) = upload_single(&srv, "text/plain", b"hello", false).await;
    assert_eq!(status, 200);
    let id = body["blob_id"].as_str().expect("blob_id must be a string");
    assert!(!id.is_empty(), "blob_id must not be empty");
}

// ── 4: caller-supplied blob_id is preserved ───────────────────────────────────
#[tokio::test]
async fn test_04_caller_supplied_blob_id_is_preserved() {
    let srv = TestServer::new().await;
    let custom_id = "my-custom-blob-id-42";
    let body = json!({
        "blob_id":      custom_id,
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "payload",
        "chunk_index":  0,
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 200);
    let j = body_json(resp).await;
    assert_eq!(j["blob_id"].as_str().unwrap(), custom_id);
}

// ── 5: missing mime_type returns 422 ─────────────────────────────────────────
#[tokio::test]
async fn test_05_missing_mime_type_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({ "enc": "none", "data": "hello", "total_chunks": 1 });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
}

// ── 6: unknown MIME type returns 422 with error key ──────────────────────────
#[tokio::test]
async fn test_06_unknown_mime_type_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":    "application/x-absolutely-unknown",
        "enc":          "base64",
        "data":         b64_encode(b"data"),
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
    let j = body_json(resp).await;
    assert_eq!(j["error"].as_str().unwrap(), "unknown_media_type");
}

// ── 7: binary MIME without base64 enc returns 422 ────────────────────────────
#[tokio::test]
async fn test_07_binary_mime_without_base64_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":    "image/jpeg",
        "enc":          "none",
        "data":         "raw-data",
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

// ── 8: invalid base64 data returns 422 ───────────────────────────────────────
#[tokio::test]
async fn test_08_invalid_base64_returns_422() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":    "image/png",
        "enc":          "base64",
        "data":         "!!!NOT-VALID-BASE64!!!",
        "total_chunks": 1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    assert_eq!(resp.status().as_u16(), 422);
    let j = body_json(resp).await;
    assert_eq!(j["error"].as_str().unwrap(), "invalid_base64");
}

// ── 9: blob meta endpoint returns correct fields ──────────────────────────────
#[tokio::test]
async fn test_09_blob_meta_returns_correct_fields() {
    let srv = TestServer::new().await;
    let (_, body) = upload_single(&srv, "image/png", b"\x89PNG\r\n\x1a\n", true).await;
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

// ── 10: blob meta for unknown id returns 404 ─────────────────────────────────
#[tokio::test]
async fn test_10_blob_meta_unknown_id_returns_404() {
    let srv = TestServer::new().await;
    let resp = call(&srv.app, get("/api/bus/blobs/no-such-blob")).await;
    assert_eq!(resp.status().as_u16(), 404);
}

// ── 11: blob list includes all uploaded blobs ─────────────────────────────────
#[tokio::test]
async fn test_11_blob_list_includes_uploaded_blobs() {
    let srv = TestServer::new().await;
    let (_, b1) = upload_single(&srv, "text/plain", b"alpha", false).await;
    let (_, b2) = upload_single(&srv, "text/markdown", b"beta", false).await;
    let id1 = b1["blob_id"].as_str().unwrap().to_string();
    let id2 = b2["blob_id"].as_str().unwrap().to_string();

    let resp = call(&srv.app, get("/api/bus/blobs")).await;
    assert_eq!(resp.status().as_u16(), 200);
    let list = body_json(resp).await;
    let ids: Vec<&str> = list["blobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    assert!(ids.contains(&id1.as_str()), "id1 missing from list");
    assert!(ids.contains(&id2.as_str()), "id2 missing from list");
}

// ── 12: delete removes blob from meta and download ───────────────────────────
#[tokio::test]
async fn test_12_delete_removes_blob() {
    let srv = TestServer::new().await;
    let (_, body) = upload_single(&srv, "text/plain", b"to be deleted", false).await;
    let blob_id = body["blob_id"].as_str().unwrap().to_string();

    let del = call(&srv.app, delete(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(del.status().as_u16(), 200);

    let meta = call(&srv.app, get(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(
        meta.status().as_u16(),
        404,
        "meta should be 404 after delete"
    );

    let (dl_status, _) = download(&srv, &blob_id).await;
    assert_eq!(dl_status, 404, "download should be 404 after delete");
}

// ── 13: delete of unknown blob returns 404 ───────────────────────────────────
#[tokio::test]
async fn test_13_delete_unknown_blob_returns_404() {
    let srv = TestServer::new().await;
    let resp = call(&srv.app, delete("/api/bus/blobs/ghost-blob-xyz")).await;
    assert_eq!(resp.status().as_u16(), 404);
}

// ── 14: TTL expiry returns 410 ───────────────────────────────────────────────
#[tokio::test]
async fn test_14_expired_blob_returns_410() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "short-lived",
        "total_chunks": 1,
        "ttl_seconds":  1,
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let (status, dl) = download(&srv, &blob_id).await;
    assert_eq!(status, 410, "expected 410 Gone, got {status}: {dl}");
    assert_eq!(dl["error"].as_str().unwrap(), "blob_expired");
}

// ── 15: ttl_seconds=0 means no expiry ────────────────────────────────────────
#[tokio::test]
async fn test_15_no_ttl_blob_does_not_expire() {
    let srv = TestServer::new().await;
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

// ── 16: access control denies unlisted agent ─────────────────────────────────
#[tokio::test]
async fn test_16_access_control_denies_unlisted_agent() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "restricted",
        "total_chunks":   1,
        "allowed_agents": ["natasha"],
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    let no_agent = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    assert_eq!(no_agent.status().as_u16(), 403, "no agent should be 403");

    let wrong = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download?agent=boris", blob_id)),
    )
    .await;
    assert_eq!(wrong.status().as_u16(), 403, "wrong agent should be 403");
}

// ── 17: access control allows listed agent ───────────────────────────────────
#[tokio::test]
async fn test_17_access_control_allows_listed_agent() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "for natasha",
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

// ── 18: empty allowed_agents means public ────────────────────────────────────
#[tokio::test]
async fn test_18_empty_allowed_agents_is_public() {
    let srv = TestServer::new().await;
    let body = json!({
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "public",
        "total_chunks":   1,
        "allowed_agents": [],
    });
    let resp = call(&srv.app, post_json("/api/bus/blobs/upload", &body)).await;
    let j = body_json(resp).await;
    let blob_id = j["blob_id"].as_str().unwrap().to_string();

    let ok = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    assert_eq!(ok.status().as_u16(), 200);
}

// ── 19: bus.blob_ready event fired on single-chunk completion ─────────────────
#[tokio::test]
async fn test_19_blob_ready_event_fired_on_single_chunk_completion() {
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

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let msgs_resp = call(&srv.app, get("/api/bus/messages")).await;
    assert_eq!(msgs_resp.status().as_u16(), 200);
    let msgs = body_json(msgs_resp).await;
    let found = msgs.as_array().unwrap().iter().any(|m| {
        m["type"].as_str() == Some("bus.blob_ready") && m["blob_id"].as_str() == Some(&blob_id)
    });
    assert!(
        found,
        "bus.blob_ready must appear in bus messages after upload"
    );
}

// ── 20: upload endpoint requires authentication ───────────────────────────────
#[tokio::test]
async fn test_20_upload_requires_auth() {
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
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = call(&srv.app, req).await;
    assert_eq!(resp.status().as_u16(), 401);
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests 21–28: chunked upload scenarios.
// ═════════════════════════════════════════════════════════════════════════════

// ── 21: two-chunk text upload assembles correctly ────────────────────────────
#[tokio::test]
async fn test_21_two_chunk_text_upload_assembles_correctly() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-text-two-parts";
    let part0 = b"first-half-";
    let part1 = b"second-half";
    let full: Vec<u8> = part0.iter().chain(part1.iter()).copied().collect();

    let (s0, j0) = upload_chunk(&srv, blob_id, "text/plain", part0, false, 0, 2).await;
    assert_eq!(s0, 200, "chunk 0: {j0}");
    assert_eq!(
        j0["complete"],
        json!(false),
        "blob must not be complete after chunk 0"
    );
    assert_eq!(j0["chunks_received"], json!(1));

    let (s1, j1) = upload_chunk(&srv, blob_id, "text/plain", part1, false, 1, 2).await;
    assert_eq!(s1, 200, "chunk 1: {j1}");
    assert_eq!(
        j1["complete"],
        json!(true),
        "blob must be complete after chunk 1"
    );
    assert_eq!(j1["chunks_received"], json!(2));

    let (dl_status, dl) = download(&srv, blob_id).await;
    assert_eq!(dl_status, 200, "download: {dl}");
    let returned = b64_decode(dl["data"].as_str().unwrap()).unwrap();
    assert_eq!(
        returned, full,
        "assembled data must equal concatenation of chunks"
    );
}

// ── 22: three-chunk binary upload assembles correctly ────────────────────────
#[tokio::test]
async fn test_22_three_chunk_binary_upload_assembles_correctly() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-binary-three-parts";
    let parts: [&[u8]; 3] = [b"\x00\x01\x02", b"\x03\x04\x05", b"\x06\x07\x08"];
    let full: Vec<u8> = parts.iter().flat_map(|p| p.iter().copied()).collect();

    for (i, part) in parts.iter().enumerate() {
        let (s, j) = upload_chunk(
            &srv,
            blob_id,
            "application/octet-stream",
            part,
            true,
            i as u64,
            3,
        )
        .await;
        assert_eq!(s, 200, "chunk {i}: {j}");
        let expected_complete = i == 2;
        assert_eq!(
            j["complete"],
            json!(expected_complete),
            "complete flag wrong after chunk {i}"
        );
    }

    let (dl_status, dl) = download(&srv, blob_id).await;
    assert_eq!(dl_status, 200, "download: {dl}");
    let returned = b64_decode(dl["data"].as_str().unwrap()).unwrap();
    assert_eq!(returned, full);
}

// ── 23: incomplete blob download returns 409 ─────────────────────────────────
#[tokio::test]
async fn test_23_download_of_incomplete_blob_returns_409() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-incomplete-blob";

    // Only send chunk 0 of 3 — leave it incomplete
    let (s, j) = upload_chunk(&srv, blob_id, "text/plain", b"only-first", false, 0, 3).await;
    assert_eq!(s, 200, "chunk 0: {j}");
    assert_eq!(j["complete"], json!(false));

    let (dl_status, dl) = download(&srv, blob_id).await;
    assert_eq!(
        dl_status, 409,
        "download of incomplete blob must return 409, got {dl_status}: {dl}"
    );
    assert_eq!(dl["error"].as_str().unwrap(), "upload_incomplete");
}

// ── 24: chunks_received counter increments per chunk ─────────────────────────
#[tokio::test]
async fn test_24_chunks_received_increments_per_chunk() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-counter-test";

    let (_, j0) = upload_chunk(&srv, blob_id, "text/plain", b"a", false, 0, 4).await;
    assert_eq!(j0["chunks_received"], json!(1), "after chunk 0");

    let (_, j1) = upload_chunk(&srv, blob_id, "text/plain", b"b", false, 1, 4).await;
    assert_eq!(j1["chunks_received"], json!(2), "after chunk 1");

    let (_, j2) = upload_chunk(&srv, blob_id, "text/plain", b"c", false, 2, 4).await;
    assert_eq!(j2["chunks_received"], json!(3), "after chunk 2");

    let (_, j3) = upload_chunk(&srv, blob_id, "text/plain", b"d", false, 3, 4).await;
    assert_eq!(j3["chunks_received"], json!(4), "after chunk 3");
    assert_eq!(j3["complete"], json!(true));
}

// ── 25: meta reflects partial state before all chunks arrive ─────────────────
#[tokio::test]
async fn test_25_meta_reflects_partial_state_mid_upload() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-partial-meta";

    upload_chunk(&srv, blob_id, "text/plain", b"part-one", false, 0, 3).await;
    upload_chunk(&srv, blob_id, "text/plain", b"part-two", false, 1, 3).await;

    let resp = call(&srv.app, get(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(resp.status().as_u16(), 200);
    let meta = body_json(resp).await;
    assert_eq!(
        meta["complete"],
        json!(false),
        "must not be complete with 2/3 chunks"
    );
    assert_eq!(meta["total_chunks"], json!(3));
    assert_eq!(meta["chunks_received"], json!(2));
}

// ── 26: bus.blob_ready fires only after the final chunk ──────────────────────
#[tokio::test]
async fn test_26_blob_ready_event_fires_only_after_final_chunk() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-event-timing";

    // Send first of two chunks
    upload_chunk(&srv, blob_id, "text/plain", b"alpha", false, 0, 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // bus.blob_ready must NOT appear yet
    let msgs_mid = body_json(call(&srv.app, get("/api/bus/messages")).await).await;
    let early_fire = msgs_mid.as_array().unwrap().iter().any(|m| {
        m["type"].as_str() == Some("bus.blob_ready") && m["blob_id"].as_str() == Some(blob_id)
    });
    assert!(
        !early_fire,
        "bus.blob_ready must not fire before the final chunk"
    );

    // Send final chunk
    upload_chunk(&srv, blob_id, "text/plain", b"beta", false, 1, 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Now it must appear
    let msgs_done = body_json(call(&srv.app, get("/api/bus/messages")).await).await;
    let fired = msgs_done.as_array().unwrap().iter().any(|m| {
        m["type"].as_str() == Some("bus.blob_ready") && m["blob_id"].as_str() == Some(blob_id)
    });
    assert!(
        fired,
        "bus.blob_ready must appear in bus messages after the final chunk"
    );
}

// ── 27: size_bytes reflects cumulative chunk sizes ────────────────────────────
#[tokio::test]
async fn test_27_size_bytes_reflects_cumulative_chunk_sizes() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-size-check";
    let part0 = b"0123456789"; // 10 bytes
    let part1 = b"abcdefghij"; // 10 bytes

    upload_chunk(&srv, blob_id, "text/plain", part0, false, 0, 2).await;
    upload_chunk(&srv, blob_id, "text/plain", part1, false, 1, 2).await;

    let resp = call(&srv.app, get(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(resp.status().as_u16(), 200);
    let meta = body_json(resp).await;
    assert_eq!(
        meta["size_bytes"].as_u64().unwrap(),
        (part0.len() + part1.len()) as u64,
        "size_bytes must equal total bytes across all chunks"
    );
}

// ── 28: large multi-chunk upload round-trips correctly ───────────────────────
#[tokio::test]
async fn test_28_large_multi_chunk_upload_round_trips_correctly() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-large-roundtrip";

    // Build 8 chunks of 256 bytes each = 2 048 bytes total
    let chunks: Vec<Vec<u8>> = (0u8..8).map(|i| vec![i; 256]).collect();
    let full: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
    let total = chunks.len() as u64;

    for (i, chunk) in chunks.iter().enumerate() {
        let (s, j) = upload_chunk(
            &srv,
            blob_id,
            "application/octet-stream",
            chunk,
            true,
            i as u64,
            total,
        )
        .await;
        assert_eq!(s, 200, "chunk {i}: {j}");
    }

    let (dl_status, dl) = download(&srv, blob_id).await;
    assert_eq!(dl_status, 200, "download: {dl}");
    let returned = b64_decode(dl["data"].as_str().unwrap()).unwrap();
    assert_eq!(
        returned, full,
        "round-tripped data must match original across all 8 chunks"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Tests 29–37: post-chunked-upload behaviour — delete, TTL, access control,
//              auth gating, and download-requires-auth on assembled blobs.
// ═════════════════════════════════════════════════════════════════════════════

// ── 29: multi-chunk blob can be deleted after completion ─────────────────────
#[tokio::test]
async fn test_29_completed_multi_chunk_blob_can_be_deleted() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-delete-after-complete";

    upload_chunk(&srv, blob_id, "text/plain", b"first", false, 0, 2).await;
    upload_chunk(&srv, blob_id, "text/plain", b"second", false, 1, 2).await;

    let del = call(&srv.app, delete(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(del.status().as_u16(), 200);

    let meta = call(&srv.app, get(&format!("/api/bus/blobs/{}", blob_id))).await;
    assert_eq!(
        meta.status().as_u16(),
        404,
        "meta should be 404 after delete"
    );

    let (dl_status, _) = download(&srv, blob_id).await;
    assert_eq!(dl_status, 404, "download should be 404 after delete");
}

// ── 30: multi-chunk blob blob_id appears in list after completion ─────────────
#[tokio::test]
async fn test_30_completed_multi_chunk_blob_appears_in_list() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-list-check";

    upload_chunk(&srv, blob_id, "text/plain", b"x", false, 0, 2).await;
    upload_chunk(&srv, blob_id, "text/plain", b"y", false, 1, 2).await;

    let resp = call(&srv.app, get("/api/bus/blobs")).await;
    assert_eq!(resp.status().as_u16(), 200);
    let list = body_json(resp).await;
    let ids: Vec<&str> = list["blobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["id"].as_str())
        .collect();
    assert!(
        ids.contains(&blob_id),
        "completed multi-chunk blob must appear in blob list"
    );
}

// ── 31: multi-chunk blob with TTL expires after deadline ─────────────────────
#[tokio::test]
async fn test_31_multi_chunk_blob_ttl_expiry() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-ttl-expiry";

    // First chunk carries the ttl_seconds
    let body0 = json!({
        "blob_id":      blob_id,
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "part-a",
        "chunk_index":  0,
        "total_chunks": 2,
        "ttl_seconds":  1,
    });
    call(&srv.app, post_json("/api/bus/blobs/upload", &body0)).await;

    let body1 = json!({
        "blob_id":      blob_id,
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "part-b",
        "chunk_index":  1,
        "total_chunks": 2,
    });
    call(&srv.app, post_json("/api/bus/blobs/upload", &body1)).await;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let (status, dl) = download(&srv, blob_id).await;
    assert_eq!(
        status, 410,
        "expected 410 Gone for expired multi-chunk blob, got {status}: {dl}"
    );
    assert_eq!(dl["error"].as_str().unwrap(), "blob_expired");
}

// ── 32: multi-chunk blob with ttl=0 does not expire ──────────────────────────
#[tokio::test]
async fn test_32_multi_chunk_blob_no_ttl_does_not_expire() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-no-ttl";

    let body0 = json!({
        "blob_id":      blob_id,
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "first",
        "chunk_index":  0,
        "total_chunks": 2,
        "ttl_seconds":  0,
    });
    call(&srv.app, post_json("/api/bus/blobs/upload", &body0)).await;

    upload_chunk(&srv, blob_id, "text/plain", b"second", false, 1, 2).await;

    let (status, _) = download(&srv, blob_id).await;
    assert_eq!(status, 200, "blob with ttl=0 must not expire");
}

// ── 33: access control applied to completed multi-chunk blob ─────────────────
#[tokio::test]
async fn test_33_access_control_on_completed_multi_chunk_blob() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-access-control";

    let body0 = json!({
        "blob_id":        blob_id,
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "secret-part-one",
        "chunk_index":    0,
        "total_chunks":   2,
        "allowed_agents": ["natasha"],
    });
    call(&srv.app, post_json("/api/bus/blobs/upload", &body0)).await;
    upload_chunk(&srv, blob_id, "text/plain", b"secret-part-two", false, 1, 2).await;

    // No agent → 403
    let no_agent = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download", blob_id)),
    )
    .await;
    assert_eq!(no_agent.status().as_u16(), 403);

    // Wrong agent → 403
    let wrong = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download?agent=boris", blob_id)),
    )
    .await;
    assert_eq!(wrong.status().as_u16(), 403);

    // Correct agent → 200
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

// ── 34: multi-chunk blob with multiple allowed agents ────────────────────────
#[tokio::test]
async fn test_34_multi_chunk_blob_multiple_allowed_agents() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-multi-agent-acl";

    let body0 = json!({
        "blob_id":        blob_id,
        "mime_type":      "text/plain",
        "enc":            "none",
        "data":           "shared-secret",
        "chunk_index":    0,
        "total_chunks":   2,
        "allowed_agents": ["natasha", "boris"],
    });
    call(&srv.app, post_json("/api/bus/blobs/upload", &body0)).await;
    upload_chunk(&srv, blob_id, "text/plain", b"-end", false, 1, 2).await;

    for agent in &["natasha", "boris"] {
        let resp = call(
            &srv.app,
            get(&format!(
                "/api/bus/blobs/{}/download?agent={}",
                blob_id, agent
            )),
        )
        .await;
        assert_eq!(
            resp.status().as_u16(),
            200,
            "agent {agent} should be allowed"
        );
    }

    let denied = call(
        &srv.app,
        get(&format!("/api/bus/blobs/{}/download?agent=eve", blob_id)),
    )
    .await;
    assert_eq!(
        denied.status().as_u16(),
        403,
        "unlisted agent must be denied"
    );
}

// ── 35: upload of chunk requires authentication ───────────────────────────────
#[tokio::test]
async fn test_35_chunk_upload_requires_auth() {
    use axum::body::Body;
    use axum::http::Request;
    let srv = TestServer::new().await;
    let body = json!({
        "blob_id":      "chunked-auth-check",
        "mime_type":    "text/plain",
        "enc":          "none",
        "data":         "payload",
        "chunk_index":  0,
        "total_chunks": 2,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/bus/blobs/upload")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = call(&srv.app, req).await;
    assert_eq!(resp.status().as_u16(), 401);
}

// ── 36: download of assembled multi-chunk blob requires authentication ────────
#[tokio::test]
async fn test_36_chunk_download_requires_auth() {
    use axum::body::Body;
    use axum::http::Request;
    let srv = TestServer::new().await;

    // First assemble the blob properly (with auth)
    let blob_id = "chunked-dl-auth-check";
    upload_chunk(&srv, blob_id, "text/plain", b"one", false, 0, 2).await;
    upload_chunk(&srv, blob_id, "text/plain", b"two", false, 1, 2).await;

    // Now download without the Authorization header
    let req = Request::builder()
        .method("GET")
        .uri(&format!("/api/bus/blobs/{}/download", blob_id))
        .body(Body::empty())
        .unwrap();
    let resp = call(&srv.app, req).await;
    assert_eq!(resp.status().as_u16(), 401);
}

// ── 37: bus.blob_ready event contains correct mime_type and size_bytes ────────
#[tokio::test]
async fn test_37_blob_ready_event_contains_mime_type_and_size() {
    let srv = TestServer::new().await;
    let blob_id = "chunked-event-fields";
    let part0 = b"hello-";
    let part1 = b"world";
    let expected_size = (part0.len() + part1.len()) as u64;

    upload_chunk(&srv, blob_id, "text/plain", part0, false, 0, 2).await;
    upload_chunk(&srv, blob_id, "text/plain", part1, false, 1, 2).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let msgs = body_json(call(&srv.app, get("/api/bus/messages")).await).await;
    let event = msgs
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["type"].as_str() == Some("bus.blob_ready") && m["blob_id"].as_str() == Some(blob_id)
        })
        .cloned()
        .expect("bus.blob_ready event must be present");

    assert_eq!(
        event["mime_type"].as_str().unwrap(),
        "text/plain",
        "event mime_type must match uploaded mime"
    );
    assert_eq!(
        event["size_bytes"].as_u64().unwrap(),
        expected_size,
        "event size_bytes must equal total bytes uploaded"
    );
}
