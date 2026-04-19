mod helpers;

use axum::http::{Request, StatusCode};
use axum::body::Body;
use serde_json::json;

fn no_auth_get(path: &str) -> Request<Body> {
    Request::builder().method("GET").uri(path).body(Body::empty()).unwrap()
}

fn no_auth_post_json(path: &str, body: &serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn head_req(path: &str) -> Request<Body> {
    Request::builder().method("HEAD").uri(path).body(Body::empty()).unwrap()
}

async fn write_file(srv: &helpers::TestServer, path: &str, content: &str) {
    let resp = helpers::call(
        &srv.app,
        no_auth_post_json("/api/fs/write", &json!({"path": path, "content": content})),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK, "write_file({path}) failed");
}

// ── Write / Read round-trip ───────────────────────────────────────────────────

#[tokio::test]
async fn test_write_read_roundtrip() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "hello.txt", "hello from fs test").await;

    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=hello.txt")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = helpers::body_bytes(resp).await;
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), "hello from fs test");
}

#[tokio::test]
async fn test_write_creates_parent_dirs() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        no_auth_post_json("/api/fs/write", &json!({
            "path": "deep/nested/dir/file.txt",
            "content": "nested"
        })),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["path"], "deep/nested/dir/file.txt");
}

#[tokio::test]
async fn test_write_returns_size() {
    let ts = helpers::TestServer::new().await;
    let content = "twelve bytes";
    let resp = helpers::call(
        &ts.app,
        no_auth_post_json("/api/fs/write", &json!({"path": "size.txt", "content": content})),
    ).await;
    let body = helpers::body_json(resp).await;
    assert_eq!(body["size"], content.len() as i64);
}

// ── Path traversal blocking ───────────────────────────────────────────────────

#[tokio::test]
async fn test_read_dotdot_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=../etc/passwd")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_read_absolute_path_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=/etc/passwd")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_write_dotdot_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        no_auth_post_json("/api/fs/write", &json!({"path": "../escape.txt", "content": "evil"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_write_absolute_path_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(
        &ts.app,
        no_auth_post_json("/api/fs/write", &json!({"path": "/tmp/escape.txt", "content": "evil"})),
    ).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_delete_dotdot_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/fs/delete?path=../etc/passwd")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Read 404 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_read_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=ghost.txt")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── MIME type detection ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_mime_json() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "data.json", "{}").await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=data.json")).await;
    assert_eq!(resp.headers()["content-type"], "application/json");
}

#[tokio::test]
async fn test_mime_txt() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "notes.txt", "hi").await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=notes.txt")).await;
    let ct = resp.headers()["content-type"].to_str().unwrap();
    assert!(ct.starts_with("text/plain"));
}

#[tokio::test]
async fn test_mime_md() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "readme.md", "# hi").await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=readme.md")).await;
    let ct = resp.headers()["content-type"].to_str().unwrap();
    assert!(ct.starts_with("text/plain"));
}

#[tokio::test]
async fn test_mime_html() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "page.html", "<h1>hi</h1>").await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=page.html")).await;
    let ct = resp.headers()["content-type"].to_str().unwrap();
    assert!(ct.starts_with("text/html"));
}

#[tokio::test]
async fn test_mime_unknown_is_octet_stream() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "binary.bin", "data").await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/read?path=binary.bin")).await;
    assert_eq!(resp.headers()["content-type"], "application/octet-stream");
}

// ── List ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_list_empty() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/list")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = helpers::body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert!(body["objects"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_list_returns_all_files() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "a/x.txt", "x").await;
    write_file(&ts, "b/y.txt", "y").await;

    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/list")).await;
    let body = helpers::body_json(resp).await;
    let objects = body["objects"].as_array().unwrap();
    assert_eq!(objects.len(), 2);
    let keys: Vec<&str> = objects.iter().map(|o| o["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"a/x.txt"), "expected a/x.txt in {keys:?}");
    assert!(keys.contains(&"b/y.txt"), "expected b/y.txt in {keys:?}");
}

#[tokio::test]
async fn test_list_objects_have_size_and_last_modified() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "meta.txt", "content").await;

    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/list")).await;
    let body = helpers::body_json(resp).await;
    let obj = &body["objects"][0];
    assert!(obj["size"].is_number());
    assert!(obj["lastModified"].is_string());
}

#[tokio::test]
async fn test_list_traversal_in_prefix_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, no_auth_get("/api/fs/list?prefix=../secret")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Exists (HEAD) ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_exists_present() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "probe.txt", "exists").await;
    let resp = helpers::call(&ts.app, head_req("/api/fs/exists?path=probe.txt")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_exists_absent() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, head_req("/api/fs/exists?path=ghost.txt")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_exists_traversal_rejected() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, head_req("/api/fs/exists?path=../etc/passwd")).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_requires_auth() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "secret.txt", "data").await;
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/fs/delete?path=secret.txt")
        .body(Body::empty())
        .unwrap();
    let resp = helpers::call(&ts.app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_delete_with_auth_removes_file() {
    let ts = helpers::TestServer::new().await;
    write_file(&ts, "deletable.txt", "bye").await;

    let resp = helpers::call(&ts.app, helpers::delete("/api/fs/delete?path=deletable.txt")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(helpers::body_json(resp).await["ok"], true);

    let exists = helpers::call(&ts.app, head_req("/api/fs/exists?path=deletable.txt")).await;
    assert_eq!(exists.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_not_found() {
    let ts = helpers::TestServer::new().await;
    let resp = helpers::call(&ts.app, helpers::delete("/api/fs/delete?path=ghost.txt")).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
