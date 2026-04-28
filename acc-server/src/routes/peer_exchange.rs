//! Peer-exchange routes — cross-agent key / credential handshake protocol.
//!
//! Three HTTP endpoints:
//!
//!   POST /api/peer-exchange/initiate
//!       Agent A starts a handshake session.  Returns an `exchange_id` the
//!       initiator can share with the peer, plus a server-side nonce.
//!
//!   POST /api/peer-exchange/execute
//!       Agent B uses an `exchange_id` to complete the exchange.
//!       If the session is valid the server marks it `completed` and returns
//!       the initiator's public payload.  Sessions are single-use; a second
//!       call returns 409 Conflict.
//!
//!   GET  /api/peer-exchange/rate-limit/:agent_a/:agent_b
//!       Returns current rate-limit counters for the ordered pair
//!       `(agent_a, agent_b)` so callers can decide whether to back off
//!       before they attempt an exchange.
//!
//! ## Persistence
//!
//! Sessions and failure counters are stored in the fleet SQLite database
//! (`peer_exchange_sessions` and `peer_exchange_failures` tables, added in
//! schema v6).  The store therefore survives server restarts and works
//! correctly in any single-writer deployment.
//!
//! ## Failure-rate logic
//!
//! Each directed pair `(initiator, peer)` has an independent sliding-window
//! counter backed by the `peer_exchange_failures` table.  Every *failed*
//! execute attempt (wrong nonce, already-completed session, session not found)
//! inserts one row.  When the count of rows for a pair within the last
//! `FAILURE_WINDOW_SECS` reaches or exceeds `FAILURE_RATE_THRESHOLD`
//! (default 5) the execute endpoint returns **429 Too Many Requests** with a
//! `retry_after_secs` hint.
//!
//! The rate-limit GET endpoint exposes these counters so peer agents can
//! implement cooperative back-off without hammering the server.
//!
//! ## Auth
//!
//! All three endpoints require a valid bearer token (same as every other
//! route in the fleet).

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum failures per directed pair inside the window before rate-limiting.
const FAILURE_RATE_THRESHOLD: u32 = 5;
/// Sliding-window duration in seconds for the failure counter.
const FAILURE_WINDOW_SECS: u64 = 300;
/// How long a session stays in the store after creation before it expires (s).
const SESSION_TTL_SECS: u64 = 600;
/// Suggested back-off returned in the 429 response body.
const RETRY_AFTER_SECS: u64 = 60;

// ── Wire types ───────────────────────────────────────────────────────────────

/// Request body for `POST /api/peer-exchange/initiate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitiateRequest {
    /// Name / identifier of the initiating agent.
    pub initiator: String,
    /// Intended peer agent that will call `/execute`.
    pub peer: String,
    /// Arbitrary public payload the initiator wishes to share (e.g. a DH
    /// public key, a JWT, or an opaque blob).
    pub public_payload: Value,
}

/// Response body for `POST /api/peer-exchange/initiate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitiateResponse {
    pub ok: bool,
    /// Unique token the initiator passes to the peer out-of-band.
    pub exchange_id: String,
    /// Server-generated nonce the peer must echo back when calling `/execute`
    /// (adds replay protection without requiring a shared secret).
    pub nonce: String,
    /// RFC-3339 timestamp at which the session expires and becomes unusable.
    pub expires_at: String,
}

/// Request body for `POST /api/peer-exchange/execute`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
    /// Token received from the initiator.
    pub exchange_id: String,
    /// Name / identifier of the executing peer agent.
    pub peer: String,
    /// Nonce that was returned by `/initiate` — must match verbatim.
    pub nonce: String,
}

/// Response body for `POST /api/peer-exchange/execute`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResponse {
    pub ok: bool,
    /// The public payload the initiator deposited during `/initiate`.
    pub public_payload: Value,
    /// RFC-3339 timestamp when the exchange was completed.
    pub completed_at: String,
    /// Name of the agent that initiated the exchange.
    pub initiator: String,
}

/// Response body for `GET /api/peer-exchange/rate-limit/:a/:b`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub ok: bool,
    /// Directed pair key `"<agent_a>→<agent_b>"`.
    pub pair: String,
    /// Number of failures recorded for this pair in the current window.
    pub failure_count: u32,
    /// Failures allowed before the endpoint starts returning 429.
    pub threshold: u32,
    /// Duration of the sliding window in seconds.
    pub window_secs: u64,
    /// RFC-3339 timestamp of the oldest failure in the current window,
    /// or `null` if there have been no failures.
    pub window_start: Option<String>,
    /// `true` when `failure_count >= threshold`.
    pub rate_limited: bool,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn parse_epoch(ts: &str) -> Option<u64> {
    ts.parse::<chrono::DateTime<chrono::Utc>>()
        .ok()
        .map(|dt| dt.timestamp() as u64)
}

fn now_epoch() -> u64 {
    chrono::Utc::now().timestamp() as u64
}

fn make_exchange_id() -> String {
    format!("pex-{}", uuid::Uuid::new_v4().to_string().replace('-', ""))
}

fn make_nonce() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..24).map(|_| rng.gen()).collect();
    hex::encode(bytes)
}

fn pair_key(a: &str, b: &str) -> String {
    format!("{}→{}", a, b)
}

fn expires_rfc3339() -> String {
    let dt = chrono::Utc::now()
        + chrono::Duration::seconds(SESSION_TTL_SECS as i64);
    dt.to_rfc3339()
}

fn is_expired(expires_at: &str) -> bool {
    parse_epoch(expires_at)
        .map(|e| now_epoch() > e)
        .unwrap_or(true)
}

/// Compute the RFC-3339 cutoff timestamp for the failure sliding window.
fn window_cutoff_rfc3339() -> String {
    let dt = chrono::Utc::now()
        - chrono::Duration::seconds(FAILURE_WINDOW_SECS as i64);
    dt.to_rfc3339()
}

// ── DB helpers ────────────────────────────────────────────────────────────────

/// Count live failures for `key` within the sliding window.
/// Returns `(count, oldest_timestamp_in_window)`.
fn db_failure_count(
    conn: &rusqlite::Connection,
    key: &str,
) -> (u32, Option<String>) {
    let cutoff = window_cutoff_rfc3339();
    let count: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM peer_exchange_failures \
             WHERE pair_key = ?1 AND failed_at >= ?2",
            params![key, cutoff],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0) as u32;

    let oldest: Option<String> = conn
        .query_row(
            "SELECT MIN(failed_at) FROM peer_exchange_failures \
             WHERE pair_key = ?1 AND failed_at >= ?2",
            params![key, cutoff],
            |r| r.get::<_, Option<String>>(0),
        )
        .unwrap_or(None);

    (count, oldest)
}

/// Insert one failure row for `key`.
fn db_record_failure(conn: &rusqlite::Connection, key: &str, ts: &str) {
    let _ = conn.execute(
        "INSERT INTO peer_exchange_failures (pair_key, failed_at) VALUES (?1, ?2)",
        params![key, ts],
    );
}

// ── Router ───────────────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/peer-exchange/initiate", post(handle_initiate))
        .route("/api/peer-exchange/execute", post(handle_execute))
        .route(
            "/api/peer-exchange/rate-limit/:agent_a/:agent_b",
            get(handle_rate_limit),
        )
}

// ── POST /api/peer-exchange/initiate ─────────────────────────────────────────

async fn handle_initiate(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<InitiateRequest>,
) -> impl IntoResponse {
    if !app.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    if body.initiator.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "initiator is required"})),
        )
            .into_response();
    }
    if body.peer.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "peer is required"})),
        )
            .into_response();
    }

    let exchange_id = make_exchange_id();
    let nonce = make_nonce();
    let created_at = now_rfc3339();
    let expires_at = expires_rfc3339();
    let payload_str = body.public_payload.to_string();

    let db = app.fleet_db.lock().await;
    let result = db.execute(
        "INSERT INTO peer_exchange_sessions \
         (exchange_id, initiator, peer, nonce, public_payload, created_at, expires_at, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')",
        params![
            exchange_id,
            body.initiator,
            body.peer,
            nonce,
            payload_str,
            created_at,
            expires_at,
        ],
    );

    match result {
        Ok(_) => (
            StatusCode::CREATED,
            Json(json!(InitiateResponse {
                ok: true,
                exchange_id,
                nonce,
                expires_at,
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("peer-exchange initiate DB error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}

// ── POST /api/peer-exchange/execute ──────────────────────────────────────────

async fn handle_execute(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ExecuteRequest>,
) -> impl IntoResponse {
    if !app.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    if body.exchange_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "exchange_id is required"})),
        )
            .into_response();
    }
    if body.peer.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "peer is required"})),
        )
            .into_response();
    }
    if body.nonce.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "nonce is required"})),
        )
            .into_response();
    }

    let now = now_rfc3339();
    let db = app.fleet_db.lock().await;

    // ── Look up session ───────────────────────────────────────────────────────

    struct Session {
        initiator: String,
        peer: String,
        nonce: String,
        expires_at: String,
        public_payload: String,
        status: String,
    }

    let session: Option<Session> = db
        .query_row(
            "SELECT initiator, peer, nonce, expires_at, public_payload, status \
             FROM peer_exchange_sessions WHERE exchange_id = ?1",
            params![body.exchange_id],
            |row| {
                Ok(Session {
                    initiator:      row.get(0)?,
                    peer:           row.get(1)?,
                    nonce:          row.get(2)?,
                    expires_at:     row.get(3)?,
                    public_payload: row.get(4)?,
                    status:         row.get(5)?,
                })
            },
        )
        .ok();

    // ── Session not found ─────────────────────────────────────────────────────
    let Some(session) = session else {
        // No initiator is known; track abuse under a self-pair for the peer.
        let key = pair_key(&body.peer, &body.peer);
        db_record_failure(&db, &key, &now);
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "session not found"})),
        )
            .into_response();
    };

    // ── Check rate-limit before any further validation ────────────────────────
    let key = pair_key(&session.initiator, &body.peer);
    let (failure_count, _) = db_failure_count(&db, &key);
    if failure_count >= FAILURE_RATE_THRESHOLD {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "rate_limited",
                "pair": key,
                "failure_count": failure_count,
                "threshold": FAILURE_RATE_THRESHOLD,
                "retry_after_secs": RETRY_AFTER_SECS,
            })),
        )
            .into_response();
    }

    // Helper macro: record a failure then return the given response.
    macro_rules! fail {
        ($status:expr, $body:expr) => {{
            db_record_failure(&db, &key, &now);
            return ($status, Json($body)).into_response();
        }};
    }

    // ── Validate ──────────────────────────────────────────────────────────────

    if session.status == "completed" {
        fail!(
            StatusCode::CONFLICT,
            json!({"error": "session already completed"})
        );
    }

    if is_expired(&session.expires_at) {
        fail!(
            StatusCode::GONE,
            json!({"error": "session expired"})
        );
    }

    if body.peer != session.peer {
        fail!(
            StatusCode::FORBIDDEN,
            json!({"error": "peer mismatch"})
        );
    }

    if body.nonce != session.nonce {
        fail!(
            StatusCode::FORBIDDEN,
            json!({"error": "nonce mismatch"})
        );
    }

    // ── Mark complete ─────────────────────────────────────────────────────────

    let update = db.execute(
        "UPDATE peer_exchange_sessions \
         SET status = 'completed', completed_at = ?1 \
         WHERE exchange_id = ?2",
        params![now, body.exchange_id],
    );

    match update {
        Ok(_) => {
            let public_payload: Value =
                serde_json::from_str(&session.public_payload).unwrap_or(Value::Null);
            (
                StatusCode::OK,
                Json(json!(ExecuteResponse {
                    ok: true,
                    public_payload,
                    completed_at: now,
                    initiator: session.initiator,
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("peer-exchange execute DB error: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}

// ── GET /api/peer-exchange/rate-limit/:agent_a/:agent_b ──────────────────────

async fn handle_rate_limit(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((agent_a, agent_b)): Path<(String, String)>,
) -> impl IntoResponse {
    if !app.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }

    let key = pair_key(&agent_a, &agent_b);
    let db = app.fleet_db.lock().await;
    let (failure_count, window_start) = db_failure_count(&db, &key);

    (
        StatusCode::OK,
        Json(json!(RateLimitInfo {
            ok: true,
            pair: key,
            failure_count,
            threshold: FAILURE_RATE_THRESHOLD,
            window_secs: FAILURE_WINDOW_SECS,
            window_start,
            rate_limited: failure_count >= FAILURE_RATE_THRESHOLD,
        })),
    )
        .into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestServer, body_json, call, get, post_json};
    use serde_json::json;

    // ── Helper: initiate a session and return the parsed JSON body ────────────

    async fn initiate(
        srv: &TestServer,
        initiator: &str,
        peer: &str,
        payload: Value,
    ) -> Value {
        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/initiate",
                &json!({
                    "initiator": initiator,
                    "peer": peer,
                    "public_payload": payload,
                }),
            ),
        )
        .await;
        body_json(resp).await
    }

    // ── 1. Initiate returns 201 with exchange_id, nonce, expires_at ──────────

    #[tokio::test]
    async fn test_initiate_success() {
        let srv = TestServer::new().await;
        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/initiate",
                &json!({
                    "initiator": "boris",
                    "peer":      "natasha",
                    "public_payload": {"key": "abc123"},
                }),
            ),
        )
        .await;

        assert_eq!(resp.status(), 201);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert!(body["exchange_id"].as_str().unwrap().starts_with("pex-"));
        assert!(!body["nonce"].as_str().unwrap().is_empty());
        assert!(!body["expires_at"].as_str().unwrap().is_empty());
    }

    // ── 2. Initiate rejects missing `initiator` with 400 ─────────────────────

    #[tokio::test]
    async fn test_initiate_missing_initiator() {
        let srv = TestServer::new().await;
        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/initiate",
                &json!({
                    "initiator": "",
                    "peer":      "natasha",
                    "public_payload": {},
                }),
            ),
        )
        .await;
        assert_eq!(resp.status(), 400);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("initiator"));
    }

    // ── 3. Execute completes a valid session ──────────────────────────────────

    #[tokio::test]
    async fn test_execute_success() {
        let srv = TestServer::new().await;
        let init_body = initiate(&srv, "boris", "natasha", json!({"secret": "hello"})).await;

        let exchange_id = init_body["exchange_id"].as_str().unwrap().to_string();
        let nonce = init_body["nonce"].as_str().unwrap().to_string();

        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/execute",
                &json!({
                    "exchange_id": exchange_id,
                    "peer":        "natasha",
                    "nonce":       nonce,
                }),
            ),
        )
        .await;

        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["public_payload"]["secret"], "hello");
        assert_eq!(body["initiator"], "boris");
        assert!(!body["completed_at"].as_str().unwrap().is_empty());
    }

    // ── 4. Execute with wrong nonce returns 403 and increments failure ────────

    #[tokio::test]
    async fn test_execute_wrong_nonce() {
        let srv = TestServer::new().await;
        let init_body = initiate(&srv, "boris", "natasha", json!({})).await;
        let exchange_id = init_body["exchange_id"].as_str().unwrap().to_string();

        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/execute",
                &json!({
                    "exchange_id": exchange_id,
                    "peer":        "natasha",
                    "nonce":       "wrongnonce",
                }),
            ),
        )
        .await;
        assert_eq!(resp.status(), 403);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("nonce"));
    }

    // ── 5. Execute on an already-completed session returns 409 ────────────────

    #[tokio::test]
    async fn test_execute_already_completed() {
        let srv = TestServer::new().await;
        let init_body = initiate(&srv, "boris", "natasha", json!({"x": 1})).await;
        let exchange_id = init_body["exchange_id"].as_str().unwrap().to_string();
        let nonce = init_body["nonce"].as_str().unwrap().to_string();

        // First call — success
        call(
            &srv.app,
            post_json(
                "/api/peer-exchange/execute",
                &json!({
                    "exchange_id": &exchange_id,
                    "peer":        "natasha",
                    "nonce":       &nonce,
                }),
            ),
        )
        .await;

        // Second call — must get 409
        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/execute",
                &json!({
                    "exchange_id": exchange_id,
                    "peer":        "natasha",
                    "nonce":       nonce,
                }),
            ),
        )
        .await;
        assert_eq!(resp.status(), 409);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("already completed"));
    }

    // ── 6. Execute with unknown exchange_id returns 404 ───────────────────────

    #[tokio::test]
    async fn test_execute_unknown_session() {
        let srv = TestServer::new().await;
        let resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/execute",
                &json!({
                    "exchange_id": "pex-doesnotexist",
                    "peer":        "natasha",
                    "nonce":       "anynonce",
                }),
            ),
        )
        .await;
        assert_eq!(resp.status(), 404);
    }

    // ── 6a. Pair-key symmetry within a single shared coordinator ─────────────
    //
    // This test validates that the directed pair key produced by `pair_key` is
    // consistent when the failure counter is written via the `/execute` path
    // and then read back via the `/rate-limit` endpoint — both against the
    // *same* `AppState` (the single in-memory fleet DB created by
    // `TestServer::new()`).
    //
    // SCOPE LIMITATION — what this test does NOT cover:
    //
    // The production server runs a single `Arc<AppState>` that is shared
    // between the HTTP request handlers, the bus-event subscriber task, and
    // the session-expiry poll loop.  This test only exercises the key
    // serialisation / look-up symmetry within one in-process store; it does
    // not verify that two *independent* coordinator instances (e.g. two
    // separate `AppState` objects that each hold their own SQLite connection)
    // would observe each other's failure records — because in production there
    // is only ever one such instance.  A future integration test that spins up
    // two server processes sharing a single on-disk database would be needed
    // to cover the cross-process scenario.

    #[tokio::test]
    async fn test_rate_limit_pair_key_symmetry() {
        let srv = TestServer::new().await;

        // alice initiates a session intended for bob.
        let init_body = initiate(&srv, "alice", "bob", json!({})).await;
        let exchange_id = init_body["exchange_id"].as_str().unwrap().to_string();

        // Record a failure on the directed pair alice→bob by submitting a
        // wrong nonce.
        let exec_resp = call(
            &srv.app,
            post_json(
                "/api/peer-exchange/execute",
                &json!({
                    "exchange_id": exchange_id,
                    "peer":        "bob",
                    "nonce":       "wrong-nonce",
                }),
            ),
        )
        .await;
        assert_eq!(exec_resp.status(), 403, "bad nonce should return 403");

        // Reading the rate-limit counter for alice→bob must reflect the
        // failure that was just recorded.
        let fwd_resp = call(
            &srv.app,
            get("/api/peer-exchange/rate-limit/alice/bob"),
        )
        .await;
        assert_eq!(fwd_resp.status(), 200);
        let fwd_body = body_json(fwd_resp).await;
        assert_eq!(fwd_body["pair"], "alice→bob",
            "pair key should be the directed string alice→bob");
        assert_eq!(fwd_body["failure_count"], 1,
            "alice→bob failure count should be 1 after one bad execute");

        // The reverse direction bob→alice is a *different* directed pair and
        // must have zero failures — confirming that the key is NOT symmetric.
        // This is the within-coordinator boundary the test covers: a single
        // AppState correctly distinguishes alice→bob from bob→alice.
        let rev_resp = call(
            &srv.app,
            get("/api/peer-exchange/rate-limit/bob/alice"),
        )
        .await;
        assert_eq!(rev_resp.status(), 200);
        let rev_body = body_json(rev_resp).await;
        assert_eq!(rev_body["pair"], "bob→alice",
            "pair key should be the directed string bob→alice");
        assert_eq!(rev_body["failure_count"], 0,
            "bob→alice must have zero failures; the counter is directed, not symmetric");
    }

    // ── 7. Rate-limit endpoint returns zeros for a fresh pair ─────────────────

    #[tokio::test]
    async fn test_rate_limit_fresh_pair() {
        let srv = TestServer::new().await;
        let resp = call(
            &srv.app,
            get("/api/peer-exchange/rate-limit/boris/natasha"),
        )
        .await;
        assert_eq!(resp.status(), 200);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["failure_count"], 0);
        assert_eq!(body["rate_limited"], false);
        assert_eq!(body["threshold"], FAILURE_RATE_THRESHOLD as u64);
        assert_eq!(body["pair"], "boris→natasha");
    }

    // ── 8. Repeated failures eventually trigger rate-limiting (429) ───────────

    #[tokio::test]
    async fn test_rate_limiting_kicks_in() {
        let srv = TestServer::new().await;

        // Initiate once so we have a valid session to probe with wrong nonces.
        let init_body = initiate(&srv, "boris", "natasha", json!({})).await;
        let exchange_id = init_body["exchange_id"].as_str().unwrap().to_string();

        // Drive failure_count up to threshold.
        // Each bad-nonce request increments the counter by 1.
        let mut last_status = 0u16;
        for i in 0..=FAILURE_RATE_THRESHOLD {
            let resp = call(
                &srv.app,
                post_json(
                    "/api/peer-exchange/execute",
                    &json!({
                        "exchange_id": exchange_id,
                        "peer":        "natasha",
                        "nonce":       format!("bad-nonce-{}", i),
                    }),
                ),
            )
            .await;
            last_status = resp.status().as_u16();
        }
        // The final call (at or beyond threshold) must be 429.
        assert_eq!(last_status, 429, "expected 429 after exceeding failure threshold");

        // The rate-limit endpoint should also report rate_limited=true now.
        let rl_resp = call(
            &srv.app,
            get("/api/peer-exchange/rate-limit/boris/natasha"),
        )
        .await;
        let rl_body = body_json(rl_resp).await;
        assert_eq!(rl_body["rate_limited"], true);
    }

    // ── 9. Execute on an expired session returns 410 Gone ────────────────────
    //
    // The handler checks `is_expired(&session.expires_at)` after the session
    // is found but before any further validation, and returns 410 Gone when
    // the session's `expires_at` timestamp lies in the past.  We exercise
    // this path by bypassing `/initiate` and inserting a row directly into
    // the fleet DB with an `expires_at` already in the past, then calling
    // `/execute` with the matching `exchange_id` and `nonce`.

    #[tokio::test]
    async fn test_execute_expired_session_returns_410() {
        use crate::testing::make_state;
        use crate::build_app;
        use tempfile::tempdir;

        // Build an AppState we can poke at directly.
        let tmp = tempdir().expect("tempdir");
        let state = make_state(&tmp).await;
        let app = build_app(state.clone());

        // Insert a session whose expires_at is firmly in the past.
        let exchange_id = "pex-expired-test-session";
        let nonce = "fixed-nonce-for-expired-test";
        let past_ts = (chrono::Utc::now() - chrono::Duration::seconds(3600)).to_rfc3339();
        let created_at = (chrono::Utc::now() - chrono::Duration::seconds(7200)).to_rfc3339();

        {
            let db = state.fleet_db.lock().await;
            db.execute(
                "INSERT INTO peer_exchange_sessions \
                 (exchange_id, initiator, peer, nonce, public_payload, created_at, expires_at, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')",
                rusqlite::params![
                    exchange_id,
                    "alice",
                    "bob",
                    nonce,
                    r#"{"key":"value"}"#,
                    created_at,
                    past_ts,
                ],
            )
            .expect("insert expired session");
        }

        // Call /execute — the session exists but is expired, so expect 410.
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

        assert_eq!(resp.status(), 410, "expired session must return 410 Gone");
        let body = body_json(resp).await;
        assert_eq!(
            body["error"].as_str().unwrap(),
            "session expired",
            "error message must be 'session expired'"
        );
    }

    // ── 10. Unauthenticated requests are rejected with 401 ────────────────────

    #[tokio::test]
    async fn test_unauthenticated_rejected() {
        use axum::body::Body;
        use axum::http::Request;

        let srv = TestServer::new().await;

        // Initiate without auth
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
        assert_eq!(resp.status(), 401);

        // Execute without auth
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
        assert_eq!(resp2.status(), 401);

        // Rate-limit GET without auth
        let req3 = Request::builder()
            .method("GET")
            .uri("/api/peer-exchange/rate-limit/a/b")
            .body(Body::empty())
            .unwrap();
        let resp3 = call(&srv.app, req3).await;
        assert_eq!(resp3.status(), 401);
    }
}
