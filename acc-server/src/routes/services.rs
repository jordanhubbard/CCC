/// /api/services/status — probe all services and return health, latency, online status.
/// Also exposes /api/presence for agent online/away/offline status.

use axum::{
    extract::State,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use crate::AppState;

fn build_catalog() -> Vec<(String, String, String, String)> {
    let hub = std::env::var("AGENT_NAME").unwrap_or_else(|_| "hub".to_string());
    let ccc_port = std::env::var("ACC_PORT").unwrap_or_else(|_| "8789".to_string());
    let tokenhub_port = std::env::var("TOKENHUB_PORT").unwrap_or_else(|_| "8090".to_string());
    let public_host = std::env::var("PUBLIC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let mut entries = vec![
        ("acc-server".to_string(),  "ACC Server".to_string(),
         format!("http://{public_host}:{ccc_port}/health"), hub.clone()),
        ("tokenhub".to_string(), "Tokenhub".to_string(),
         format!("http://127.0.0.1:{tokenhub_port}/health"), hub.clone()),
        ("agentbus".to_string(), "AgentBus".to_string(),
         format!("http://127.0.0.1:{ccc_port}/api/health"), hub.clone()),
    ];
    // Optional Qdrant
    if let Ok(qdrant_url) = std::env::var("QDRANT_ADDRESS") {
        entries.push(("qdrant".to_string(), "Qdrant".to_string(),
            format!("{qdrant_url}/"), hub.clone()));
    }
    entries
}

const CACHE_TTL_SECS: u64 = 30;
const PROBE_TIMEOUT_MS: u64 = 3000;

struct ServiceCache {
    data: Option<Vec<Value>>,
    fetched_at: Option<Instant>,
}

static CACHE: std::sync::OnceLock<RwLock<ServiceCache>> = std::sync::OnceLock::new();

fn cache() -> &'static RwLock<ServiceCache> {
    CACHE.get_or_init(|| RwLock::new(ServiceCache { data: None, fetched_at: None }))
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/services/status", get(services_status))
        .route("/api/presence", get(presence))
}

async fn services_status() -> impl IntoResponse {
    // Return cached if fresh
    {
        let c = cache().read().await;
        if let (Some(data), Some(ts)) = (&c.data, c.fetched_at) {
            if ts.elapsed() < Duration::from_secs(CACHE_TTL_SECS) {
                return Json(data.clone());
            }
        }
        // Stale cache: return stale immediately and trigger background refresh
        if let Some(data) = &c.data {
            let stale = data.clone();
            drop(c);
            tokio::spawn(async { refresh_cache().await; });
            return Json(stale);
        }
    }
    // Cold start: must wait for probe
    let results = probe_all().await;
    let mut c = cache().write().await;
    c.data = Some(results.clone());
    c.fetched_at = Some(Instant::now());
    Json(results)
}

async fn refresh_cache() {
    let results = probe_all().await;
    let mut c = cache().write().await;
    c.data = Some(results);
    c.fetched_at = Some(Instant::now());
}

async fn probe_all() -> Vec<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(PROBE_TIMEOUT_MS))
        .build()
        .unwrap_or_default();

    let catalog = build_catalog();
    let probes: Vec<_> = catalog.into_iter().map(|(id, name, url, host)| {
        let client = client.clone();
        async move {
            let start = Instant::now();
            let online = client.get(&url).send().await
                .map(|r| r.status().is_success() || r.status().as_u16() < 500)
                .unwrap_or(false);
            let latency_ms = if online { Some(start.elapsed().as_millis() as u64) } else { None };
            json!({
                "id": id,
                "name": name,
                "url": url,
                "host": host,
                "online": online,
                "latency_ms": latency_ms,
            })
        }
    }).collect();

    futures_util::future::join_all(probes).await
}

// ── /api/presence ─────────────────────────────────────────────────────────

const PRESENCE_ONLINE_MS:  i64 = 3 * 60 * 1000;
const PRESENCE_AWAY_MS:    i64 = 15 * 60 * 1000;

async fn presence(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let agents = state.agents.read().await;
    let now = chrono::Utc::now().timestamp_millis();
    let mut presence_map = serde_json::Map::new();

    if let Some(agent_obj) = agents.as_object() {
        for (name, agent) in agent_obj {
            if agent.get("decommissioned").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }
            let last_seen = agent.get("lastSeen")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let gap_ms: Option<i64> = last_seen.as_deref().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s).ok()
                    .map(|dt| now - dt.timestamp_millis())
            });
            let status = match gap_ms {
                None => "unknown",
                Some(g) if g <= PRESENCE_ONLINE_MS => "online",
                Some(g) if g <= PRESENCE_AWAY_MS   => "away",
                _ => "offline",
            };
            let task = agent.get("currentTask")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let gpu = agent.get("capabilities").and_then(|c| c.get("gpu")).cloned();
            let status_text = match status {
                "online" => {
                    let base = task.as_deref()
                        .map(|t| format!("busy: {}", &t[..t.len().min(40)]))
                        .unwrap_or_else(|| "idle".to_string());
                    if let Some(g) = &gpu {
                        format!("{} · {}", base, g.as_str().unwrap_or(""))
                    } else { base }
                }
                other => other.to_string(),
            };
            presence_map.insert(name.clone(), json!({
                "status": status,
                "statusText": status_text,
                "since": last_seen,
                "host": agent.get("host").cloned(),
                "gpu": gpu,
                "gap_minutes": gap_ms.map(|g| g / 60_000),
            }));
        }
    }

    Json(json!({
        "ok": true,
        "agents": presence_map,
        "ts": chrono::Utc::now().to_rfc3339(),
    }))
}
