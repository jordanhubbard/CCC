//! Integration tests for the peer-exchange HTTP API.
//!
//! Covers every route exposed by `routes/peer_exchange.rs`:
//!
//! * `POST /api/peer-exchange/initiate`
//! * `POST /api/peer-exchange/execute`
//! * `GET  /api/peer-exchange/rate-limit/:agent_a/:agent_b`
//!
//! Each test runs against an in-process `TestServer` with an in-memory SQLite
//! database, so there is no I/O to disk and tests can run in parallel.
//!
//! # Coverage map
//!
//! | # | Endpoint | Scenario |
//! |---|---|---|
//! |  1 | POST /initiate     | Happy path — 201 with exchange_id / nonce / expires_at |
//! |  2 | POST /initiate     | Missing `initiator` field → 400 |
//! |  3 | POST /initiate     | Missing `peer` field → 400 |
//! |  4 | POST /initiate     | Blank `initiator` (whitespace only) → 400 |
//! |  5 | POST /execute      | Happy path — completes a valid pending session → 200 |
//! |  6 | POST /execute      | Wrong nonce → 403 with "nonce mismatch" |
//! |  7 | POST /execute      | Unknown exchange_id → 404 |
//! |  8 | POST /execute      | Already-completed session → 409 |
//! |  9 | POST /execute      | Peer name mismatch → 403 |
//! | 10 | POST /execute      | Missing `exchange_id` → 400 |
//! | 11 | POST /execute      | Missing `nonce` → 400 |
//! | 12 | GET  /rate-limit   | Fresh pair → 200, failure_count 0, rate_limited false |
//! | 13 | GET  /rate-limit   | Failure counter increments after bad-nonce execute |
//! | 14 | GET  /rate-limit   | rate_limited true once failure count ≥ threshold |
//! | 15 | POST /execute      | 429 returned when failure count ≥ threshold |
//! | 16 | All endpoints      | Unauthenticated requests → 401 |
//! | 17 | POST /initiate     | `public_payload` is preserved through execute |
//! | 18 | POST /initiate     | Unique exchange_ids across two sessions |
//! | 19 | GET  /rate-limit   | Directed pair key is order-sensitive (a→b ≠ b→a) |
//! | 20 | POST /execute      | Expired session → 410 Gone |

use acc_server::testing::{TestServer, body_json, call, get, post_json};
use serde_json::json;

// ── Shared helper ─────────────────────────────────────────────────────────────

/// POST /api/peer-exchange/initiate and return the parsed JSON body.
async fn initiate(
    srv: &TestServer,
    initiator: &str,
    peer: &str,
    payload: serde_json::Value,
) -> serde_json::Value {
    let resp = call(
        &srv.app,
        post_json(
            "/api/peer-exchange/initiate",
            &json!({
                "initiator":      initiator,
                "peer":           peer,
                "public_payload": payload,
            }),
        ),
    )
    .await;
    body_json(resp).await
}

/// POST /api/peer-exchange/execute and return the raw response.
async fn execute_raw(
    srv: &TestServer,
    exchange_id: &str,
    peer: &str,
    nonce: &str,
) -> axum::http::Response<axum::body::Body> {
    call(
        &srv.app,
        post_json(
            "/api/peer-exchange/execute",
            &json!({
                "exchange_id": exchange_id,
                "peer":        peer,
                "nonce":       nonce,
            }),
        ),
    )
    .await
}

// ── 1. Happy-path initiate ────────────────────────────────────────────────────

#[tokio::test]
async fn test_01_initiate_success_returns_201_with_required_fields() {
    let srv = TestServer::new().await;

    let resp = call(
        &srv.app,
        post_json(
            "/api/peer-exchange/initiate",
            &json!({
                "initiator":      "boris",
                "peer":           "natasha",
                "public_payload": {"key": "abc123"},
            }),
        ),
    )
    .await;

    assert_eq!(resp.status(), 201, "initiate must return 201 Created");

    let body = body_json(resp).await;
    assert_eq!(body["ok"], true, "ok must be true");

    let exchange_id = body["exchange_id"].as_str().expect("exchange_id must be a string");
    assert!(
        exchange_id.starts_with("pex-"),
        "exchange_id must start with 'pex-', got {exchange_id}"
    );

    assert!(
        !body["nonce"].as_str().unwrap_or("").is_empty(),
        "nonce must not be empty"
    );
    assert!(
        !body["expires_at"].as_str().unwrap_or("").is_empty(),
        "expires_at must not be empty"
    );
}

// ── 2. Missing initiator → 400 ────────────────────────────────────────────────

#[tokio::test]
async fn test_02_initiate_missing_initiator_returns_400() {
    let srv = TestServer::new().await;

    let resp = call(
        &srv.app,
        post_json(
            "/api/peer-exchange/initiate",
            &json!({
                "initiator":      "",
                "peer":           "natasha",
                "public_payload": {},
            }),
        ),
    )
    .await;

    assert_eq!(resp.status(), 400, "blank initiator must return 400");
    let body = body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("initiator"),
        "error must mention 'initiator'"
    );
}

// ── 3. Missing peer → 400 ────────────────────────────────────────────────────

#[tokio::test]
async fn test_03_initiate_missing_peer_returns_400() {
    let srv = TestServer::new().await;

    let resp = call(
        &srv.app,
        post_json(
            "/api/peer-exchange/initiate",
            &json!({
                "initiator":      "boris",
                "peer":           "",
                "public_payload": {},
            }),
        ),
    )
    .await;

    assert_eq!(resp.status(), 400, "blank peer must return 400");
    let body = body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("peer"),
        "error must mention 'peer'"
    );
}

// ── 4. Whitespace-only initiator → 400 ───────────────────────────────────────

#[tokio::test]
async fn test_04_initiate_whitespace_initiator_returns_400() {
    let srv = TestServer::new().await;

    let resp = call(
        &srv.app,
        post_json(
            "/api/peer-exchange/initiate",
            &json!({
                "initiator":      "   ",
                "peer":           "natasha",
                "public_payload": {},
            }),
        ),
    )
    .await;

    assert_eq!(
        resp.status(),
        400,
        "whitespace-only initiator must return 400"
    );
}

// ── 5. Happy-path execute ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_05_execute_success_completes_session() {
    let srv = TestServer::new().await;

    let init = initiate(&srv, "boris", "natasha", json!({"secret": "hello"})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();
    let nonce = init["nonce"].as_str().unwrap().to_string();

    let resp = execute_raw(&srv, &exchange_id, "natasha", &nonce).await;
    assert_eq!(resp.status(), 200, "valid execute must return 200");

    let body = body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["public_payload"]["secret"], "hello");
    assert_eq!(body["initiator"], "boris");
    assert!(
        !body["completed_at"].as_str().unwrap_or("").is_empty(),
        "completed_at must be set"
    );
}

// ── 6. Wrong nonce → 403 ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_06_execute_wrong_nonce_returns_403() {
    let srv = TestServer::new().await;

    let init = initiate(&srv, "boris", "natasha", json!({})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();

    let resp = execute_raw(&srv, &exchange_id, "natasha", "wrong-nonce").await;
    assert_eq!(resp.status(), 403, "wrong nonce must return 403");

    let body = body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap_or("").contains("nonce"),
        "error must mention nonce mismatch"
    );
}

// ── 7. Unknown exchange_id → 404 ──────────────────────────────────────────────

#[tokio::test]
async fn test_07_execute_unknown_exchange_id_returns_404() {
    let srv = TestServer::new().await;

    let resp = execute_raw(&srv, "pex-does-not-exist", "natasha", "anynonce").await;
    assert_eq!(resp.status(), 404, "unknown exchange_id must return 404");
}

// ── 8. Already-completed session → 409 ───────────────────────────────────────

#[tokio::test]
async fn test_08_execute_already_completed_returns_409() {
    let srv = TestServer::new().await;

    let init = initiate(&srv, "boris", "natasha", json!({"x": 1})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();
    let nonce = init["nonce"].as_str().unwrap().to_string();

    // First execute — succeeds
    let first = execute_raw(&srv, &exchange_id, "natasha", &nonce).await;
    assert_eq!(first.status(), 200);

    // Second execute — must be 409 Conflict
    let second = execute_raw(&srv, &exchange_id, "natasha", &nonce).await;
    assert_eq!(second.status(), 409, "second execute must return 409 Conflict");

    let body = body_json(second).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("already completed"),
        "error must mention 'already completed'"
    );
}

// ── 9. Peer name mismatch → 403 ───────────────────────────────────────────────

#[tokio::test]
async fn test_09_execute_peer_mismatch_returns_403() {
    let srv = TestServer::new().await;

    let init = initiate(&srv, "boris", "natasha", json!({})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();
    let nonce = init["nonce"].as_str().unwrap().to_string();

    // Claim to be "bullwinkle" but session is for "natasha"
    let resp = execute_raw(&srv, &exchange_id, "bullwinkle", &nonce).await;
    assert_eq!(resp.status(), 403, "peer mismatch must return 403");

    let body = body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap_or("").contains("peer"),
        "error must mention peer mismatch"
    );
}

// ── 10. Missing exchange_id → 400 ────────────────────────────────────────────

#[tokio::test]
async fn test_10_execute_missing_exchange_id_returns_400() {
    let srv = TestServer::new().await;

    let resp = call(
        &srv.app,
        post_json(
            "/api/peer-exchange/execute",
            &json!({
                "exchange_id": "",
                "peer":        "natasha",
                "nonce":       "anynonce",
            }),
        ),
    )
    .await;

    assert_eq!(
        resp.status(),
        400,
        "blank exchange_id must return 400"
    );
}

// ── 11. Missing nonce → 400 ───────────────────────────────────────────────────

#[tokio::test]
async fn test_11_execute_missing_nonce_returns_400() {
    let srv = TestServer::new().await;

    let resp = call(
        &srv.app,
        post_json(
            "/api/peer-exchange/execute",
            &json!({
                "exchange_id": "pex-abc",
                "peer":        "natasha",
                "nonce":       "",
            }),
        ),
    )
    .await;

    assert_eq!(resp.status(), 400, "blank nonce must return 400");
}

// ── 12. Rate-limit endpoint — fresh pair ─────────────────────────────────────

#[tokio::test]
async fn test_12_rate_limit_fresh_pair_returns_zeros() {
    let srv = TestServer::new().await;

    let resp = call(
        &srv.app,
        get("/api/peer-exchange/rate-limit/boris/natasha"),
    )
    .await;

    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["failure_count"], 0, "fresh pair must have 0 failures");
    assert_eq!(
        body["rate_limited"], false,
        "fresh pair must not be rate-limited"
    );
    assert_eq!(
        body["pair"], "boris→natasha",
        "pair key must be directed"
    );
}

// ── 13. Failure counter increments on bad-nonce execute ──────────────────────

#[tokio::test]
async fn test_13_rate_limit_increments_on_bad_nonce() {
    let srv = TestServer::new().await;

    // Seed a session
    let init = initiate(&srv, "alice", "bob", json!({})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();

    // Send one bad-nonce execute to generate a failure
    let bad = execute_raw(&srv, &exchange_id, "bob", "definitely-wrong").await;
    assert_eq!(bad.status(), 403);

    // The rate-limit counter for alice→bob must now be 1
    let rl_resp = call(
        &srv.app,
        get("/api/peer-exchange/rate-limit/alice/bob"),
    )
    .await;
    assert_eq!(rl_resp.status(), 200);
    let body = body_json(rl_resp).await;
    assert_eq!(
        body["failure_count"], 1,
        "failure_count must be 1 after one bad execute"
    );
}

// ── 14. rate_limited true once failure count ≥ threshold ─────────────────────

#[tokio::test]
async fn test_14_rate_limit_true_after_threshold_exceeded() {
    let srv = TestServer::new().await;

    // Seed a session that we will use to hammer bad nonces
    let init = initiate(&srv, "alice", "bob", json!({})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();

    // Drive failures up to and past the threshold (5)
    for i in 0..6 {
        execute_raw(&srv, &exchange_id, "bob", &format!("bad-nonce-{i}")).await;
    }

    let rl_resp = call(
        &srv.app,
        get("/api/peer-exchange/rate-limit/alice/bob"),
    )
    .await;
    assert_eq!(rl_resp.status(), 200);
    let body = body_json(rl_resp).await;
    assert_eq!(
        body["rate_limited"], true,
        "rate_limited must be true after exceeding threshold"
    );
    assert!(
        body["failure_count"].as_u64().unwrap_or(0) >= 5,
        "failure_count must be at least 5"
    );
}

// ── 15. 429 returned when failure count ≥ threshold ──────────────────────────

#[tokio::test]
async fn test_15_execute_returns_429_when_rate_limited() {
    let srv = TestServer::new().await;

    let init = initiate(&srv, "alice", "bob", json!({})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();

    // Hit failure threshold (5) and expect a 429 on the next attempt
    let mut last_status = 0u16;
    for i in 0..=5u32 {
        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/execute",
                &json!({
                    "exchange_id": exchange_id,
                    "peer":        "bob",
                    "nonce":       format!("bad-nonce-{i}"),
                }),
            ),
        )
        .await;
        last_status = resp.status().as_u16();
    }

    assert_eq!(
        last_status, 429,
        "must return 429 after the failure threshold is reached"
    );
}

// ── 16. All endpoints reject unauthenticated requests ────────────────────────

#[tokio::test]
async fn test_16_unauthenticated_requests_return_401() {
    use axum::body::Body;
    use axum::http::Request;

    let srv = TestServer::new().await;

    // POST /initiate without auth
    let req = Request::builder()
        .method("POST")
        .uri("/api/peer-exchange/initiate")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "initiator": "eve",
                "peer": "bob",
                "public_payload": {}
            })
            .to_string(),
        ))
        .unwrap();
    let resp = call(&srv.app, req).await;
    assert_eq!(resp.status(), 401, "initiate without auth must be 401");

    // POST /execute without auth
    let req2 = Request::builder()
        .method("POST")
        .uri("/api/peer-exchange/execute")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "exchange_id": "pex-any",
                "peer": "bob",
                "nonce": "x"
            })
            .to_string(),
        ))
        .unwrap();
    let resp2 = call(&srv.app, req2).await;
    assert_eq!(resp2.status(), 401, "execute without auth must be 401");

    // GET /rate-limit without auth
    let req3 = Request::builder()
        .method("GET")
        .uri("/api/peer-exchange/rate-limit/a/b")
        .body(Body::empty())
        .unwrap();
    let resp3 = call(&srv.app, req3).await;
    assert_eq!(
        resp3.status(),
        401,
        "rate-limit GET without auth must be 401"
    );
}

// ── 17. public_payload round-trips through the exchange ──────────────────────

#[tokio::test]
async fn test_17_public_payload_is_preserved() {
    let srv = TestServer::new().await;

    let payload = json!({
        "algorithm": "x25519",
        "public_key": "deadbeef0102030405060708090a0b0c",
        "extra": [1, 2, 3],
    });

    let init = initiate(&srv, "alice", "bob", payload.clone()).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();
    let nonce = init["nonce"].as_str().unwrap().to_string();

    let exec = body_json(execute_raw(&srv, &exchange_id, "bob", &nonce).await).await;

    assert_eq!(
        exec["public_payload"]["algorithm"], "x25519",
        "algorithm field must be preserved"
    );
    assert_eq!(
        exec["public_payload"]["public_key"],
        "deadbeef0102030405060708090a0b0c",
        "public_key field must be preserved"
    );
    assert_eq!(
        exec["public_payload"]["extra"],
        json!([1, 2, 3]),
        "extra array must be preserved"
    );
}

// ── 18. Two initiations produce distinct exchange_ids ────────────────────────

#[tokio::test]
async fn test_18_unique_exchange_ids_per_initiation() {
    let srv = TestServer::new().await;

    let init1 = initiate(&srv, "alice", "bob", json!({})).await;
    let init2 = initiate(&srv, "alice", "carol", json!({})).await;

    let id1 = init1["exchange_id"].as_str().unwrap();
    let id2 = init2["exchange_id"].as_str().unwrap();

    assert_ne!(
        id1, id2,
        "each initiation must produce a unique exchange_id"
    );
}

// ── 19. Directed pair key is order-sensitive (a→b ≠ b→a) ────────────────────

#[tokio::test]
async fn test_19_rate_limit_pair_key_is_directed() {
    let srv = TestServer::new().await;

    // Seed a failure on alice→bob
    let init = initiate(&srv, "alice", "bob", json!({})).await;
    let exchange_id = init["exchange_id"].as_str().unwrap().to_string();
    execute_raw(&srv, &exchange_id, "bob", "wrong-nonce-abc").await;

    // alice→bob must have 1 failure
    let fwd = body_json(
        call(
            &srv.app,
            get("/api/peer-exchange/rate-limit/alice/bob"),
        )
        .await,
    )
    .await;
    assert_eq!(
        fwd["failure_count"], 1,
        "alice→bob must have exactly 1 failure"
    );
    assert_eq!(fwd["pair"], "alice→bob");

    // bob→alice is a different directed key and must have 0 failures
    let rev = body_json(
        call(
            &srv.app,
            get("/api/peer-exchange/rate-limit/bob/alice"),
        )
        .await,
    )
    .await;
    assert_eq!(
        rev["failure_count"], 0,
        "bob→alice must have 0 failures (directed key is not symmetric)"
    );
    assert_eq!(rev["pair"], "bob→alice");
}

// ── 20. Expired session → 410 Gone ───────────────────────────────────────────
//
// We bypass `/initiate` and insert a session row directly with an
// `expires_at` already in the past, then verify that `/execute` returns 410.

#[tokio::test]
async fn test_20_execute_expired_session_returns_410() {
    use tempfile::tempdir;

    // Build a fresh state so we can poke the DB directly.
    let tmp = tempdir().expect("tempdir");
    let state = acc_server::testing::make_state(&tmp).await;
    let app = acc_server::build_app(state.clone());

    let exchange_id = "pex-expired-integration-test";
    let nonce = "fixed-nonce-for-expired-session";
    let past_ts =
        (chrono::Utc::now() - chrono::Duration::seconds(3600)).to_rfc3339();
    let created_ts =
        (chrono::Utc::now() - chrono::Duration::seconds(7200)).to_rfc3339();

    {
        let db = state.fleet_db.lock().await;
        db.execute(
            "INSERT INTO peer_exchange_sessions \
             (exchange_id, initiator, peer, nonce, public_payload, \
              created_at, expires_at, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')",
            rusqlite::params![
                exchange_id,
                "alice",
                "bob",
                nonce,
                r#"{"key":"value"}"#,
                created_ts,
                past_ts,
            ],
        )
        .expect("insert expired session");
    }

    let resp = call(
        &app,
        post_json(
            "/api/peer-exchange/execute",
            &json!({
                "exchange_id": exchange_id,
                "peer":        "bob",
                "nonce":       nonce,
            }),
        ),
    )
    .await;

    assert_eq!(
        resp.status(),
        410,
        "expired session must return 410 Gone"
    );

    let body = body_json(resp).await;
    assert_eq!(
        body["error"].as_str().unwrap_or(""),
        "session expired",
        "error message must be 'session expired'"
    );
}
