/// User auth routes.
///
/// Admin endpoints (guarded by agent token):
///   GET    /api/auth/users              — list all users
///   POST   /api/auth/users              — create user, returns token once
///   DELETE /api/auth/users/:username    — revoke user
///
/// Public endpoints:
///   POST   /api/auth/login              — validate username + token
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use rand::Rng;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/users", get(list_users).post(create_user))
        .route("/api/auth/users/:username", delete(delete_user))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::thread_rng().gen();
    format!("ccc-{}", hex::encode(bytes))
}

// ── Login ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    token: String,
}

#[derive(Serialize)]
struct LoginResponse {
    ok: bool,
    username: String,
}

async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    let token_hash = hash_token(&body.token);
    let db = state.auth_db.lock().await;
    let found = db
        .query_row(
            "SELECT username FROM users WHERE username = ?1 AND token_hash = ?2",
            params![body.username, token_hash],
            |row| row.get::<_, String>(0),
        )
        .ok();

    match found {
        Some(username) => {
            let now = chrono::Utc::now().to_rfc3339();
            let _ = db.execute(
                "UPDATE users SET last_seen = ?1 WHERE username = ?2",
                params![now, username],
            );
            Ok(Json(LoginResponse { ok: true, username }))
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

// ── List users ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct UserEntry {
    id: String,
    username: String,
    created_at: String,
    last_seen: Option<String>,
}

async fn list_users(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<UserEntry>>, StatusCode> {
    if !state.is_admin_authed(&headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let db = state.auth_db.lock().await;
    let mut stmt = db
        .prepare("SELECT id, username, created_at, last_seen FROM users ORDER BY created_at")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let users: Vec<UserEntry> = stmt
        .query_map([], |row| {
            Ok(UserEntry {
                id: row.get(0)?,
                username: row.get(1)?,
                created_at: row.get(2)?,
                last_seen: row.get(3)?,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Json(users))
}

// ── Create user ───────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CreateUserRequest {
    username: String,
}

#[derive(Serialize)]
struct CreateUserResponse {
    username: String,
    /// Plaintext token — shown exactly once. Store it somewhere safe.
    token: String,
}

async fn create_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<CreateUserResponse>), (StatusCode, Json<serde_json::Value>)> {
    if !state.is_admin_authed(&headers) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Unauthorized"})),
        ));
    }

    let token = generate_token();
    let token_hash = hash_token(&token);
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    {
        let db = state.auth_db.lock().await;
        db.execute(
            "INSERT INTO users (id, username, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, body.username, token_hash, now],
        )
        .map_err(|e| {
            let msg = e.to_string();
            let status = if msg.contains("UNIQUE") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, Json(serde_json::json!({"error": msg})))
        })?;
    }

    // Update in-memory cache
    state
        .user_token_hashes
        .write()
        .unwrap()
        .insert(token_hash);

    tracing::info!("Created user: {}", body.username);
    Ok((
        StatusCode::CREATED,
        Json(CreateUserResponse {
            username: body.username,
            token,
        }),
    ))
}

// ── Delete user ───────────────────────────────────────────────────────────────

async fn delete_user(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    if !state.is_admin_authed(&headers) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "Unauthorized"})),
        ));
    }

    let token_hash: Option<String> = {
        let db = state.auth_db.lock().await;
        db.query_row(
            "SELECT token_hash FROM users WHERE username = ?1",
            params![username],
            |row| row.get(0),
        )
        .ok()
    };

    {
        let db = state.auth_db.lock().await;
        let affected = db
            .execute("DELETE FROM users WHERE username = ?1", params![username])
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
            })?;
        if affected == 0 {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "User not found"})),
            ));
        }
    }

    if let Some(hash) = token_hash {
        state.user_token_hashes.write().unwrap().remove(&hash);
    }

    tracing::info!("Deleted user: {}", username);
    Ok(StatusCode::NO_CONTENT)
}
