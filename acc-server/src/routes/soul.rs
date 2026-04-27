/// Soul serialization, transfer, and agent move operations.
///
/// An agent "soul" is the complete serialized identity of an agent:
///   - Registry entry (capabilities, slack_id, billing, token, etc.)
///   - Host data: tar.gz of ~/.hermes/ and ~/.acc/ (hex-encoded), sent by the agent
///     in response to a soul.export bus event.
///
/// Routes:
///   GET  /api/agents/:name/soul            — export soul (triggers agent export, returns cached)
///   POST /api/agents/:name/soul/data       — agent uploads its packaged host files
///   POST /api/agents/move                  — move agent identity to a different host
use crate::state::db_flush_agents;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/agents/:name/soul",        get(get_soul))
        .route("/api/agents/:name/soul/data",   post(post_soul_data))
        .route("/api/agents/move",              post(move_agent))
}

// ── GET /api/agents/:name/soul ────────────────────────────────────────────────

async fn get_soul(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    let agent_name = name.to_lowercase();
    let agents = state.agents.read().await;
    let registry = match agents.as_object().and_then(|m| m.get(&agent_name)) {
        Some(a) => a.clone(),
        None => return (StatusCode::NOT_FOUND, Json(json!({"error":"Agent not found"}))).into_response(),
    };
    drop(agents);

    // Trigger a fresh host export from the agent via bus event
    let export_event = json!({
        "type": "soul.export",
        "to": agent_name,
        "from": "server",
        "body": {}
    });
    let event_str = serde_json::to_string(&export_event).unwrap_or_default();
    let _ = state.bus_tx.send(event_str);

    // Return registry + any previously cached host data
    let soul_store = state.soul_store.read().await;
    let host_data = soul_store.get(&agent_name).cloned();
    drop(soul_store);

    Json(json!({
        "ok": true,
        "soul": {
            "agent": agent_name,
            "registry": registry,
            "host_data": host_data,
            "host_data_status": if host_data.is_some() { "ready" } else { "pending" },
        }
    })).into_response()
}

// ── POST /api/agents/:name/soul/data ─────────────────────────────────────────

/// Called by an agent in response to a soul.export bus event.
/// Body: { "agent": "boris", "tar_gz_hex": "...", "exported_at": "..." }
async fn post_soul_data(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    let agent_name = name.to_lowercase();
    let mut soul_store = state.soul_store.write().await;
    soul_store.insert(agent_name.clone(), body);
    drop(soul_store);

    Json(json!({"ok": true, "agent": agent_name})).into_response()
}

// ── POST /api/agents/move ─────────────────────────────────────────────────────

/// Moves source agent's soul onto target host.
///
/// Body: { "source": "boris", "target": "ollama", "decommission_source": true }
///
/// What transfers (from source → merged onto target):
///   identity: name, token, slack_id, capabilities, billing, tags
/// What stays from target:
///   connectivity: host, tailscale_ip, ssh, gpu, ram telemetry
///
/// After the move:
///   - Target agent restarts with source name and config
///   - Source registry entry is decommissioned (or deleted)
///   - Source agent receives soul.decommission event and exits
async fn move_agent(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }

    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some(s) => s.to_lowercase(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error":"source required"}))).into_response(),
    };
    let target = match body.get("target").and_then(|v| v.as_str()) {
        Some(t) => t.to_lowercase(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error":"target required"}))).into_response(),
    };
    let decommission = body.get("decommission_source")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if source == target {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"source and target must differ"}))).into_response();
    }

    // Validate both agents exist
    {
        let agents = state.agents.read().await;
        let map = agents.as_object().unwrap();
        if !map.contains_key(&source) {
            return (StatusCode::NOT_FOUND, Json(json!({"error": format!("source agent '{}' not found", source)}))).into_response();
        }
        if !map.contains_key(&target) {
            return (StatusCode::NOT_FOUND, Json(json!({"error": format!("target agent '{}' not found", target)}))).into_response();
        }
    }

    // Step 1: trigger soul.export from source, wait up to 30s
    {
        state.soul_store.write().await.remove(&source);
        let event_str = serde_json::to_string(&json!({
            "type": "soul.export",
            "to": source,
            "from": "server",
            "body": {"reason": "move"}
        })).unwrap_or_default();
        let _ = state.bus_tx.send(event_str);
    }

    let host_data = wait_for_soul(&state, &source, 30).await;

    // Step 2: merge identity onto target
    {
        let mut agents = state.agents.write().await;
        let map = agents.as_object_mut().unwrap();

        let source_entry = map.get(&source).cloned().unwrap_or(json!({}));
        let target_entry = map.get(&target).cloned().unwrap_or(json!({}));

        // Identity fields from source
        let identity_keys = ["name", "token", "slack_id", "capabilities", "billing",
                             "tags", "registeredAt", "type"];
        // Connectivity/hardware from target
        let hw_keys = ["host", "tailscale_ip", "ssh", "gpu", "gpu_temp_c", "gpu_power_w",
                       "gpu_util_pct", "ram", "ram_used_mb", "ram_avail_mb", "ram_total_mb",
                       "unified_vram_used_mb", "unified_vram_free_mb", "unified_vram_total_mb",
                       "ollama_status", "ollama_models", "ccc_version", "lastSeen"];

        let mut merged = serde_json::Map::new();
        for key in &identity_keys {
            if let Some(v) = source_entry.get(*key) {
                merged.insert((*key).to_string(), v.clone());
            }
        }
        for key in &hw_keys {
            if let Some(v) = target_entry.get(*key) {
                merged.insert((*key).to_string(), v.clone());
            }
        }
        merged.insert("name".into(), json!(source));

        // Replace target entry with merged (it will now respond as source name)
        map.remove(&target);
        map.insert(source.clone(), Value::Object(merged));

        // Decommission old source entry if it was separate
        if decommission {
            // source entry has been replaced by the merged one above
            // Mark a tombstone so the slot is gone
        }
        drop(agents);
        db_flush_agents(&state).await;
    }

    // Step 3: send soul.import to target machine (agent will restart as source name)
    let new_token = {
        let agents = state.agents.read().await;
        agents.as_object()
            .and_then(|m| m.get(&source))
            .and_then(|a| a.get("token"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    let tar_hex = host_data.as_ref()
        .and_then(|h| h.get("tar_gz_hex"))
        .cloned()
        .unwrap_or(json!(null));
    let import_payload = json!({
        "new_name": source,
        "new_token": new_token,
        "tar_gz_hex": tar_hex,
    });

    let import_event = json!({
        "type": "soul.import",
        "to": target,
        "from": "server",
        "body": import_payload,
    });
    let _ = state.bus_tx.send(serde_json::to_string(&import_event).unwrap_or_default());

    // Step 4: tell source to decommission itself
    if decommission {
        let decomm_event = json!({
            "type": "soul.decommission",
            "to": source,
            "from": "server",
            "body": {"reason": format!("moved to {}", target)},
        });
        let _ = state.bus_tx.send(serde_json::to_string(&decomm_event).unwrap_or_default());
    }

    Json(json!({
        "ok": true,
        "source": source,
        "target": target,
        "host_data_transferred": host_data.is_some(),
        "decommissioned_source": decommission,
        "status": "soul.import sent to target — agent will restart with new identity",
    })).into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn wait_for_soul(state: &Arc<AppState>, agent: &str, timeout_secs: u64) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        {
            let store = state.soul_store.read().await;
            if let Some(data) = store.get(agent) {
                return Some(data.clone());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    None
}
