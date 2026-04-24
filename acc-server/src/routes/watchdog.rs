//! Watchdog — detects agents that go offline while holding claimed/in-progress tasks.
//!
//! Runs as a background tokio task (spawned from main.rs).  Every
//! `WATCHDOG_INTERVAL_SECS` (default 120) it:
//!
//!   1. Reads the agents registry to find any agent whose `lastSeen` is older
//!      than `WATCHDOG_OFFLINE_THRESHOLD_SECS` (default 600 = 10 min).
//!   2. Queries fleet_tasks for rows still claimed_by that agent with status
//!      in ('claimed', 'in_progress').
//!   3. For each match, emits an `agent.abandoned_work` bus event and logs it.
//!
//! A per-agent cooldown (`WATCHDOG_ALERT_COOLDOWN_SECS`, default 900 = 15 min)
//! prevents repeated alerts for the same agent while they remain offline.
//!
//! Endpoints:
//!   GET /api/watchdog/status  — current watchdog state (last run, alerts fired)
//!   GET /api/watchdog/alerts  — recent abandoned-work alerts

use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::AppState;

// ── Configuration (env-overridable) ─────────────────────────────────────────

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn interval_secs() -> u64 { env_u64("WATCHDOG_INTERVAL_SECS", 120) }
fn offline_threshold_secs() -> u64 { env_u64("WATCHDOG_OFFLINE_THRESHOLD_SECS", 600) }
fn alert_cooldown_secs() -> u64 { env_u64("WATCHDOG_ALERT_COOLDOWN_SECS", 900) }

// ── Shared watchdog state ───────────────────────────────────────────────────

#[derive(Clone)]
pub struct WatchdogState {
    inner: Arc<RwLock<WatchdogInner>>,
}

struct WatchdogInner {
    last_run: Option<DateTime<Utc>>,
    total_runs: u64,
    total_alerts: u64,
    /// Per-agent cooldown: agent_name → last alert time
    cooldowns: HashMap<String, DateTime<Utc>>,
    /// Recent alerts (ring buffer, last 50)
    recent_alerts: Vec<Value>,
}

impl WatchdogState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(WatchdogInner {
                last_run: None,
                total_runs: 0,
                total_alerts: 0,
                cooldowns: HashMap::new(),
                recent_alerts: Vec::new(),
            })),
        }
    }
}

// ── Routes ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/watchdog/status", get(get_status))
        .route("/api/watchdog/alerts", get(get_alerts))
}

async fn get_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    let wd = &state.watchdog;
    let inner = wd.inner.read().await;
    Json(json!({
        "ok": true,
        "last_run": inner.last_run.map(|t| t.to_rfc3339()),
        "total_runs": inner.total_runs,
        "total_alerts": inner.total_alerts,
        "active_cooldowns": inner.cooldowns.len(),
        "recent_alert_count": inner.recent_alerts.len(),
        "config": {
            "interval_secs": interval_secs(),
            "offline_threshold_secs": offline_threshold_secs(),
            "alert_cooldown_secs": alert_cooldown_secs(),
        },
    })).into_response()
}

async fn get_alerts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    let wd = &state.watchdog;
    let inner = wd.inner.read().await;
    Json(json!({
        "ok": true,
        "alerts": inner.recent_alerts,
        "count": inner.recent_alerts.len(),
    })).into_response()
}

// ── Background task ─────────────────────────────────────────────────────────

pub async fn run_watchdog(state: Arc<AppState>) {
    let interval = interval_secs();
    let threshold = offline_threshold_secs();
    let cooldown = alert_cooldown_secs();

    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    tracing::info!(
        "watchdog: started (interval={}s, offline_threshold={}s, cooldown={}s)",
        interval, threshold, cooldown
    );

    // Skip first immediate tick to let the server finish starting
    ticker.tick().await;

    loop {
        ticker.tick().await;
        if let Err(e) = watchdog_tick(&state, threshold, cooldown).await {
            tracing::warn!("watchdog: tick error: {}", e);
        }
    }
}

async fn watchdog_tick(
    state: &Arc<AppState>,
    threshold_secs: u64,
    cooldown_secs: u64,
) -> Result<(), String> {
    let now = Utc::now();

    // 1. Find offline agents
    let offline_agents = find_offline_agents(state, now, threshold_secs).await;

    // 2. For each offline agent, check for abandoned tasks
    let mut alerts = Vec::new();
    for (agent_name, last_seen, offline_secs) in &offline_agents {
        let abandoned = find_abandoned_tasks(state, agent_name).await?;
        if !abandoned.is_empty() {
            alerts.push((agent_name.clone(), last_seen.clone(), *offline_secs, abandoned));
        }
    }

    // 3. Fire alerts (respecting cooldowns)
    let wd = &state.watchdog;
    let mut inner = wd.inner.write().await;
    inner.last_run = Some(now);
    inner.total_runs += 1;

    // Expire old cooldowns
    let cooldown_dur = chrono::Duration::seconds(cooldown_secs as i64);
    inner.cooldowns.retain(|_, ts| now.signed_duration_since(*ts) < cooldown_dur);

    for (agent_name, last_seen, offline_secs, tasks) in &alerts {
        // Check cooldown
        if inner.cooldowns.contains_key(agent_name) {
            tracing::debug!("watchdog: {} still in cooldown, skipping alert", agent_name);
            continue;
        }

        let task_summaries: Vec<Value> = tasks.iter().map(|t| {
            json!({
                "id": t.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                "title": t.get("title").and_then(|v| v.as_str()).unwrap_or("?"),
                "project_id": t.get("project_id").and_then(|v| v.as_str()).unwrap_or("?"),
                "status": t.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
                "claimed_at": t.get("claimed_at").cloned().unwrap_or(json!(null)),
            })
        }).collect();

        let alert = json!({
            "type": "agent.abandoned_work",
            "agent": agent_name,
            "last_seen": last_seen,
            "offline_seconds": offline_secs,
            "abandoned_tasks": task_summaries,
            "task_count": tasks.len(),
            "ts": now.to_rfc3339(),
        });

        tracing::warn!(
            "watchdog: {} offline {}s with {} abandoned task(s)",
            agent_name, offline_secs, tasks.len()
        );

        // Emit bus event
        let _ = state.bus_tx.send(alert.to_string());

        // Record
        inner.cooldowns.insert(agent_name.clone(), now);
        inner.total_alerts += 1;
        inner.recent_alerts.push(alert);

        // Keep ring buffer at 50
        if inner.recent_alerts.len() > 50 {
            inner.recent_alerts.remove(0);
        }
    }

    Ok(())
}

/// Returns (agent_name, last_seen_rfc3339, offline_seconds) for each offline agent.
async fn find_offline_agents(
    state: &Arc<AppState>,
    now: DateTime<Utc>,
    threshold_secs: u64,
) -> Vec<(String, String, u64)> {
    let agents = state.agents.read().await;
    let mut result = Vec::new();

    if let Some(map) = agents.as_object() {
        for (name, agent) in map {
            // Skip decommissioned agents
            if agent.get("decommissioned").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }

            if let Some(ts_str) = agent.get("lastSeen").and_then(|v| v.as_str()) {
                if let Ok(ts) = ts_str.parse::<DateTime<Utc>>() {
                    let age = now.signed_duration_since(ts);
                    if age.num_seconds() > threshold_secs as i64 {
                        result.push((
                            name.clone(),
                            ts_str.to_string(),
                            age.num_seconds() as u64,
                        ));
                    }
                }
            }
        }
    }

    result
}

/// Query fleet_tasks for tasks claimed by `agent_name` that are still claimed or in_progress.
async fn find_abandoned_tasks(
    state: &Arc<AppState>,
    agent_name: &str,
) -> Result<Vec<Value>, String> {
    let db = state.fleet_db.lock().await;

    let mut stmt = db.prepare(
        "SELECT id, project_id, title, status, claimed_by, claimed_at, task_type \
         FROM fleet_tasks \
         WHERE claimed_by = ?1 AND status IN ('claimed', 'in_progress')"
    ).map_err(|e| format!("prepare: {e}"))?;

    let rows = stmt.query_map(rusqlite::params![agent_name], |row| {
        Ok(json!({
            "id":               row.get::<_, String>(0)?,
            "project_id":       row.get::<_, String>(1)?,
            "title":            row.get::<_, String>(2)?,
            "status":           row.get::<_, String>(3)?,
            "claimed_by":       row.get::<_, Option<String>>(4)?,
            "claimed_at":       row.get::<_, Option<String>>(5)?,
            "task_type":        row.get::<_, String>(6).unwrap_or_else(|_| "work".to_string()),
        }))
    }).map_err(|e| format!("query: {e}"))?;

    let mut tasks = Vec::new();
    for row in rows {
        if let Ok(task) = row {
            tasks.push(task);
        }
    }

    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_defaults() {
        // Defaults when env vars not set
        assert_eq!(interval_secs(), 120);
        assert_eq!(offline_threshold_secs(), 600);
        assert_eq!(alert_cooldown_secs(), 900);
    }

    #[test]
    fn test_watchdog_state_new() {
        let state = WatchdogState::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let inner = state.inner.read().await;
            assert!(inner.last_run.is_none());
            assert_eq!(inner.total_runs, 0);
            assert_eq!(inner.total_alerts, 0);
            assert!(inner.cooldowns.is_empty());
            assert!(inner.recent_alerts.is_empty());
        });
    }
}
