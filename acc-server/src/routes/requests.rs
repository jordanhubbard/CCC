//! User request pool — first-responder coordination across the fleet.
//!
//! When a user sends a request (via Slack, Telegram, web, etc.) it lands here.
//! All listening agents race to claim it via POST /api/requests/:id/claim.
//! The atomic SQL WHERE-clause ensures exactly one agent wins; the rest get 409
//! and back off to their own queued work.
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/requests", get(list_requests).post(create_request))
        .route("/api/requests/:id", get(get_request))
        .route("/api/requests/:id/claim", post(claim_request))
        .route("/api/requests/:id/complete", put(complete_request))
}

fn row_to_request(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let meta_str: String = row.get(10)?;
    let metadata: Value = serde_json::from_str(&meta_str).unwrap_or(json!({}));
    Ok(json!({
        "id":           row.get::<_, String>(0)?,
        "body":         row.get::<_, String>(1)?,
        "channel":      row.get::<_, String>(2)?,
        "status":       row.get::<_, String>(3)?,
        "claimed_by":   row.get::<_, Option<String>>(4)?,
        "claimed_at":   row.get::<_, Option<String>>(5)?,
        "completed_at": row.get::<_, Option<String>>(6)?,
        "completed_by": row.get::<_, Option<String>>(7)?,
        "created_at":   row.get::<_, String>(8)?,
        "updated_at":   row.get::<_, String>(9)?,
        "metadata":     metadata,
    }))
}

#[derive(Deserialize)]
struct RequestQuery {
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_requests(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<RequestQuery>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let db = state.fleet_db.lock().await;
    let mut sql = String::from(
        "SELECT id,body,channel,status,claimed_by,claimed_at,completed_at,completed_by,created_at,updated_at,metadata FROM requests WHERE 1=1"
    );
    let mut binds: Vec<String> = vec![];

    if let Some(s) = &q.status {
        sql.push_str(" AND status=?");
        binds.push(s.clone());
    }
    sql.push_str(" ORDER BY created_at DESC");
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));

    let mut stmt = match db.prepare(&sql) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":e.to_string()}))).into_response(),
    };
    let requests: Vec<Value> = stmt
        .query_map(rusqlite::params_from_iter(binds.iter().map(|s| s.as_str())), row_to_request)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    let count = requests.len();
    Json(json!({"requests": requests, "count": count})).into_response()
}

async fn get_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let db = state.fleet_db.lock().await;
    match db.query_row(
        "SELECT id,body,channel,status,claimed_by,claimed_at,completed_at,completed_by,created_at,updated_at,metadata FROM requests WHERE id=?1",
        params![id],
        row_to_request,
    ) {
        Ok(r) => Json(r).into_response(),
        Err(rusqlite::Error::QueryReturnedNoRows) =>
            (StatusCode::NOT_FOUND, Json(json!({"error":"Request not found"}))).into_response(),
        Err(e) =>
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":e.to_string()}))).into_response(),
    }
}

async fn create_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let text = body.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let channel = body.get("channel").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let metadata = body.get("metadata").map(|v| v.to_string()).unwrap_or_else(|| "{}".to_string());
    let id = format!("req-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));

    let db = state.fleet_db.lock().await;
    match db.execute(
        "INSERT INTO requests (id, body, channel, metadata) VALUES (?1,?2,?3,?4)",
        params![id, text, channel, metadata],
    ) {
        Ok(_) => {
            let req = db.query_row(
                "SELECT id,body,channel,status,claimed_by,claimed_at,completed_at,completed_by,created_at,updated_at,metadata FROM requests WHERE id=?1",
                params![id],
                row_to_request,
            ).unwrap_or(json!({"id": id}));
            let _ = state.bus_tx.send(json!({"type":"user.request","request_id":id}).to_string());
            (StatusCode::CREATED, Json(json!({"ok":true,"request":req}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":e.to_string()}))).into_response(),
    }
}

async fn claim_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let agent = match body.get("agent").and_then(|v| v.as_str()) {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error":"agent required"}))).into_response(),
    };

    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();

    // Atomic claim: only one agent wins; second gets 0 rows changed → 409
    let rows = db.execute(
        "UPDATE requests SET status='claimed', claimed_by=?1, claimed_at=?2, updated_at=?2 WHERE id=?3 AND status='pending'",
        params![agent, now, id],
    ).unwrap_or(0);

    if rows == 0 {
        let exists: bool = db.query_row(
            "SELECT COUNT(*) FROM requests WHERE id=?1", params![id], |r| r.get::<_, i64>(0)
        ).unwrap_or(0) > 0;
        return if exists {
            (StatusCode::CONFLICT, Json(json!({"error":"already_claimed"}))).into_response()
        } else {
            (StatusCode::NOT_FOUND, Json(json!({"error":"Request not found"}))).into_response()
        };
    }

    let req = db.query_row(
        "SELECT id,body,channel,status,claimed_by,claimed_at,completed_at,completed_by,created_at,updated_at,metadata FROM requests WHERE id=?1",
        params![id],
        row_to_request,
    ).unwrap_or(json!({"id":id}));

    let _ = state.bus_tx.send(json!({"type":"user.request.claimed","request_id":id,"agent":agent}).to_string());
    (StatusCode::OK, Json(json!({"ok":true,"request":req}))).into_response()
}

async fn complete_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let agent = body.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let db = state.fleet_db.lock().await;
    let now = chrono::Utc::now().to_rfc3339();
    let rows = db.execute(
        "UPDATE requests SET status='completed', completed_at=?1, completed_by=?2, updated_at=?1 WHERE id=?3 AND status IN ('claimed','pending')",
        params![now, agent, id],
    ).unwrap_or(0);
    if rows == 0 {
        return (StatusCode::NOT_FOUND, Json(json!({"error":"Request not found or already completed"}))).into_response();
    }
    let req = db.query_row(
        "SELECT id,body,channel,status,claimed_by,claimed_at,completed_at,completed_by,created_at,updated_at,metadata FROM requests WHERE id=?1",
        params![id],
        row_to_request,
    ).unwrap_or(json!({"id":id}));
    let _ = state.bus_tx.send(json!({"type":"user.request.completed","request_id":id,"agent":agent}).to_string());
    Json(json!({"ok":true,"request":req})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestServer, body_json, call, get, post_json};
    use axum::http::Request;
    use axum::body::Body;

    fn put_json(path: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri(path)
            .header("Authorization", format!("Bearer {}", crate::testing::TEST_TOKEN))
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn test_create_and_list_request() {
        let srv = TestServer::new().await;
        let resp = call(&srv.app, post_json("/api/requests", &json!({"body":"help me","channel":"slack"}))).await;
        assert_eq!(resp.status(), 201);
        let body = body_json(resp).await;
        assert_eq!(body["request"]["status"], "pending");
        assert_eq!(body["request"]["channel"], "slack");

        let resp2 = call(&srv.app, get("/api/requests")).await;
        let list = body_json(resp2).await;
        assert_eq!(list["count"], 1);
    }

    #[tokio::test]
    async fn test_get_request_not_found() {
        let srv = TestServer::new().await;
        let resp = call(&srv.app, get("/api/requests/nonexistent")).await;
        assert_eq!(resp.status(), 404);
    }

    #[tokio::test]
    async fn test_claim_request_success() {
        let srv = TestServer::new().await;
        let create = call(&srv.app, post_json("/api/requests", &json!({"body":"deploy prod"}))).await;
        let body = body_json(create).await;
        let id = body["request"]["id"].as_str().unwrap().to_string();

        let resp = call(&srv.app, post_json(
            &format!("/api/requests/{id}/claim"),
            &json!({"agent":"boris"}),
        )).await;
        assert_eq!(resp.status(), 200);
        let claimed = body_json(resp).await;
        assert_eq!(claimed["request"]["status"], "claimed");
        assert_eq!(claimed["request"]["claimed_by"], "boris");
    }

    #[tokio::test]
    async fn test_claim_request_conflict_on_double_claim() {
        let srv = TestServer::new().await;
        let create = call(&srv.app, post_json("/api/requests", &json!({"body":"urgent task"}))).await;
        let body = body_json(create).await;
        let id = body["request"]["id"].as_str().unwrap().to_string();

        // First claim wins
        call(&srv.app, post_json(&format!("/api/requests/{id}/claim"), &json!({"agent":"boris"}))).await;

        // Second agent gets 409
        let resp2 = call(&srv.app, post_json(
            &format!("/api/requests/{id}/claim"),
            &json!({"agent":"natasha"}),
        )).await;
        assert_eq!(resp2.status(), 409);
        let body2 = body_json(resp2).await;
        assert_eq!(body2["error"], "already_claimed");
    }

    #[tokio::test]
    async fn test_claim_missing_agent_field() {
        let srv = TestServer::new().await;
        let create = call(&srv.app, post_json("/api/requests", &json!({"body":"x"}))).await;
        let body = body_json(create).await;
        let id = body["request"]["id"].as_str().unwrap().to_string();

        let resp = call(&srv.app, post_json(&format!("/api/requests/{id}/claim"), &json!({}))).await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_complete_request() {
        let srv = TestServer::new().await;
        let create = call(&srv.app, post_json("/api/requests", &json!({"body":"do something"}))).await;
        let body = body_json(create).await;
        let id = body["request"]["id"].as_str().unwrap().to_string();

        call(&srv.app, post_json(&format!("/api/requests/{id}/claim"), &json!({"agent":"boris"}))).await;

        let resp = call(&srv.app, put_json(
            &format!("/api/requests/{id}/complete"),
            &json!({"agent":"boris"}),
        )).await;
        assert_eq!(resp.status(), 200);
        let done = body_json(resp).await;
        assert_eq!(done["request"]["status"], "completed");
        assert_eq!(done["request"]["completed_by"], "boris");
    }

    #[tokio::test]
    async fn test_list_requests_filter_by_status() {
        let srv = TestServer::new().await;
        call(&srv.app, post_json("/api/requests", &json!({"body":"r1"}))).await;
        let r2 = call(&srv.app, post_json("/api/requests", &json!({"body":"r2"}))).await;
        let b2 = body_json(r2).await;
        let id2 = b2["request"]["id"].as_str().unwrap().to_string();
        call(&srv.app, post_json(&format!("/api/requests/{id2}/claim"), &json!({"agent":"boris"}))).await;

        let resp = call(&srv.app, get("/api/requests?status=pending")).await;
        let list = body_json(resp).await;
        assert_eq!(list["count"], 1);
        assert_eq!(list["requests"][0]["status"], "pending");
    }

    #[tokio::test]
    async fn test_concurrent_claim_only_one_wins() {
        let srv = TestServer::new().await;
        let create = call(&srv.app, post_json("/api/requests", &json!({"body":"race me"}))).await;
        let body = body_json(create).await;
        let id = body["request"]["id"].as_str().unwrap().to_string();

        // Simulate two agents racing — run them sequentially here since
        // TestServer uses an in-process router. The atomic SQL guarantees
        // only one succeeds regardless of timing.
        let agents = ["boris", "natasha", "bullwinkle"];
        let mut wins = 0u32;
        let mut conflicts = 0u32;

        for agent in &agents {
            let app = srv.app.clone();
            let resp = call(&app, post_json(
                &format!("/api/requests/{id}/claim"),
                &json!({"agent": *agent}),
            )).await;
            match resp.status().as_u16() {
                200 => wins += 1,
                409 => conflicts += 1,
                s => panic!("unexpected status {s}"),
            }
        }

        assert_eq!(wins, 1, "exactly one agent should win");
        assert_eq!(conflicts, 2, "remaining agents should get 409");
    }
}
