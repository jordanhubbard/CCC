use crate::brain::BrainQueue;
use crate::supervisor::SupervisorHandle;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct QueueData {
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    #[serde(default)]
    pub completed: Vec<serde_json::Value>,
}

pub struct AppState {
    /// Static agent tokens from config (plaintext, never change at runtime).
    pub auth_tokens: HashSet<String>,
    /// In-memory cache of user token SHA-256 hashes (loaded from auth.db, updated on add/delete).
    pub user_token_hashes: std::sync::RwLock<HashSet<String>>,
    /// Auth SQLite database (always-on).
    pub auth_db: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    /// Fleet task pool SQLite database (always-on, single source of truth for all state).
    pub fleet_db: Arc<tokio::sync::Mutex<Connection>>,
    pub queue: RwLock<QueueData>,
    pub agents: RwLock<serde_json::Value>,
    pub secrets: RwLock<serde_json::Map<String, serde_json::Value>>,
    pub projects: RwLock<Vec<serde_json::Value>>,
    pub brain: Arc<BrainQueue>,
    pub bus_tx: broadcast::Sender<String>,
    pub bus_seq: AtomicU64,
    pub start_time: std::time::SystemTime,
    pub fs_root: String,
    pub supervisor: Option<Arc<SupervisorHandle>>,
    /// Cached soul packages keyed by agent name.
    /// Populated when an agent responds to a soul.export bus event.
    pub soul_store: RwLock<HashMap<String, serde_json::Value>>,
    /// In-memory blob metadata store. Keyed by blob_id.
    pub blob_store: RwLock<HashMap<String, crate::bus_types::BlobMeta>>,
    /// Filesystem path where blob data is stored (one file per blob_id).
    pub blobs_path: String,
    /// Path to the dead-letter queue JSONL file.
    pub dlq_path: String,
    /// Per-token role map: token (plaintext) → role string.
    /// Used to gate role-restricted endpoints at runtime.
    pub user_token_roles: std::sync::RwLock<std::collections::HashMap<String, String>>,
    /// Watchdog state: tracks abandoned-work detection and alerts.
    pub watchdog: crate::routes::watchdog::WatchdogState,
    /// Filesystem path for bus log (JSONL append-only, not state storage).
    pub bus_log_path: String,
}

impl AppState {
    /// Extract raw bearer token from Authorization header.
    pub fn bearer_token_str<'a>(&self, headers: &'a axum::http::HeaderMap) -> &'a str {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim_start_matches("Bearer ")
            .trim()
    }

    /// Find agent name by matching token against agents registry.
    pub async fn agent_from_token(&self, token: &str) -> Option<String> {
        let agents = self.agents.read().await;
        if let Some(obj) = agents.as_object() {
            for (name, agent) in obj {
                if agent.get("token").and_then(|t| t.as_str()) == Some(token) {
                    return Some(name.clone());
                }
            }
        }
        None
    }

    fn bearer_token<'a>(&self, headers: &'a axum::http::HeaderMap) -> &'a str {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim_start_matches("Bearer ")
            .trim()
    }

    /// Returns true if the request carries a valid agent token (from config).
    /// Used to gate admin-only endpoints.
    pub fn is_admin_authed(&self, headers: &axum::http::HeaderMap) -> bool {
        if self.auth_tokens.is_empty() {
            return true;
        }
        let token = self.bearer_token(headers);
        use subtle::ConstantTimeEq;
        for valid in &self.auth_tokens {
            let a: &[u8] = token.as_bytes();
            let b: &[u8] = valid.as_bytes();
            if a.len() == b.len() && bool::from(a.ct_eq(b)) {
                return true;
            }
        }
        false
    }

    /// Returns true if the request is authenticated by either an agent token or a user token.
    pub fn is_authed(&self, headers: &axum::http::HeaderMap) -> bool {
        let user_hashes = self.user_token_hashes.read().unwrap();
        if self.auth_tokens.is_empty() && user_hashes.is_empty() {
            return true; // dev mode — no tokens configured at all
        }

        // Check agent tokens (plaintext)
        if self.is_admin_authed(headers) {
            return true;
        }

        // Check user tokens (SHA-256 hash of the bearer token)
        let token = self.bearer_token(headers);
        if !token.is_empty() {
            use sha2::{Sha256, Digest};
            let mut hasher = Sha256::new();
            hasher.update(token.as_bytes());
            let hash = hex::encode(hasher.finalize());
            use subtle::ConstantTimeEq;
            for valid_hash in user_hashes.iter() {
                let a: &[u8] = hash.as_bytes();
                let b: &[u8] = valid_hash.as_bytes();
                if a.len() == b.len() && bool::from(a.ct_eq(b)) {
                    return true;
                }
            }
        }

        false
    }
}

/// Load all in-memory state from fleet_db (SQLite single source of truth).
pub async fn load_all(state: &Arc<AppState>) {
    let conn = state.fleet_db.lock().await;
    *state.agents.write().await  = crate::db::db_load_agents(&conn);
    let items     = crate::db::db_load_queue_items(&conn);
    let completed = crate::db::db_load_queue_completed(&conn);
    *state.queue.write().await   = QueueData { items, completed };
    *state.secrets.write().await = crate::db::db_load_secrets(&conn);
    *state.projects.write().await = crate::db::db_load_projects(&conn);
    tracing::info!("State loaded from SQLite (fleet_db)");
}

// ── DB flush helpers — write in-memory cache back to fleet_db ────────────────
// Each does a full DELETE + INSERT for the resource so the DB always matches
// exactly what's in memory. Fine for fleet sizes of ~5–50 agents.

pub async fn db_flush_agents(state: &Arc<AppState>) {
    let agents = state.agents.read().await;
    let conn = state.fleet_db.lock().await;
    if let Err(e) = conn.execute("DELETE FROM agents", []) {
        tracing::warn!("db_flush_agents: clear failed: {}", e);
        return;
    }
    if let Some(map) = agents.as_object() {
        for data in map.values() {
            if let Err(e) = crate::db::db_upsert_agent(&conn, data) {
                tracing::warn!("db_flush_agents: upsert failed: {}", e);
            }
        }
    }
}

pub async fn db_flush_queue(state: &Arc<AppState>) {
    let q = state.queue.read().await;
    let conn = state.fleet_db.lock().await;
    // Replace active items entirely so deletions/moves are reflected.
    if let Err(e) = conn.execute("DELETE FROM queue_items", []) {
        tracing::warn!("db_flush_queue: clear failed: {}", e);
        return;
    }
    for item in &q.items {
        if let Err(e) = crate::db::db_upsert_queue_item(&conn, item) {
            tracing::warn!("db_flush_queue: upsert item failed: {}", e);
        }
    }
    // Completed items are append-only; INSERT OR REPLACE is safe.
    for item in &q.completed {
        if let Err(e) = crate::db::db_upsert_queue_completed(&conn, item) {
            tracing::warn!("db_flush_queue: upsert completed failed: {}", e);
        }
    }
}

pub async fn db_flush_secrets(state: &Arc<AppState>) {
    let secrets = state.secrets.read().await;
    let conn = state.fleet_db.lock().await;
    if let Err(e) = conn.execute("DELETE FROM secrets", []) {
        tracing::warn!("db_flush_secrets: clear failed: {}", e);
        return;
    }
    for (key, value) in secrets.iter() {
        let val_str = value
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| value.to_string());
        if let Err(e) = crate::db::db_upsert_secret(&conn, key, &val_str) {
            tracing::warn!("db_flush_secrets: upsert failed: {}", e);
        }
    }
}

pub async fn db_flush_projects(state: &Arc<AppState>) {
    let projects = state.projects.read().await;
    let conn = state.fleet_db.lock().await;
    if let Err(e) = conn.execute("DELETE FROM projects", []) {
        tracing::warn!("db_flush_projects: clear failed: {}", e);
        return;
    }
    for project in projects.iter() {
        if let Err(e) = crate::db::db_upsert_project(&conn, project) {
            tracing::warn!("db_flush_projects: upsert failed: {}", e);
        }
    }
}
