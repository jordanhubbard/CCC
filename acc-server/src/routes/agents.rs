use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post, put},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use crate::AppState;
use crate::state::db_flush_agents;
use crate::state;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/agents", get(get_agents).post(post_agent))
        .route("/api/agents/names", get(get_agent_names))
        .route("/api/agents/register", post(register_agent))
        .route("/api/agents/:name", get(get_agent_by_name).post(upsert_agent).patch(patch_agent).delete(delete_agent))
        .route("/api/agents/:name/heartbeat", post(agent_heartbeat))
        .route("/api/agents/:name/health", get(get_agent_health))
        .route("/api/agents/:name/capabilities", put(register_tool_capabilities))
        .route("/api/heartbeat/:agent", post(post_heartbeat))
        .route("/api/heartbeats", get(get_heartbeats))
}

fn is_online(agent: &Value) -> bool {
    if let Some(ts_str) = agent.get("lastSeen").and_then(|v| v.as_str()) {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts_str) {
            let age = chrono::Utc::now().signed_duration_since(dt.with_timezone(&chrono::Utc));
            return age.num_seconds() < 300;
        }
    }
    false
}

// Derives onlineStatus from live data — never trust the stored string.
fn online_status(agent: &Value) -> &'static str {
    if agent.get("decommissioned").and_then(|v| v.as_bool()).unwrap_or(false) {
        return "decommissioned";
    }
    if is_online(agent) { "online" } else { "offline" }
}

// GET /api/agents[?online=true]
// ?online=true — return only agents whose lastSeen is within 300s
async fn get_agents(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let online_only = params.get("online").map(|v| v == "true").unwrap_or(false);
    let agents = state.agents.read().await;
    let mut result: Vec<Value> = match agents.as_object() {
        Some(map) => map.values().filter_map(|a| {
            if online_only && !is_online(a) { return None; }
            let mut record = a.clone();
            if let Some(obj) = record.as_object_mut() {
                obj.insert("online".into(), json!(is_online(a)));
                obj.insert("onlineStatus".into(), json!(online_status(a)));
            }
            Some(record)
        }).collect(),
        None => vec![],
    };
    result.sort_by(|a, b| {
        let ts_a = a.get("lastSeen").and_then(|v| v.as_str()).unwrap_or("");
        let ts_b = b.get("lastSeen").and_then(|v| v.as_str()).unwrap_or("");
        ts_b.cmp(ts_a)
    });
    Json(json!({ "ok": true, "agents": result }))
}

// GET /api/agents/names[?online=true]
// Lightweight peer-discovery endpoint — returns only names, no telemetry.
async fn get_agent_names(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let online_only = params.get("online").map(|v| v == "true").unwrap_or(false);
    let agents = state.agents.read().await;
    let names: Vec<&str> = match agents.as_object() {
        Some(map) => map.iter().filter_map(|(name, a)| {
            if online_only && !is_online(a) { return None; }
            if online_status(a) == "decommissioned" { return None; }
            Some(name.as_str())
        }).collect(),
        None => vec![],
    };
    Json(json!({ "ok": true, "names": names }))
}

async fn get_heartbeats(State(state): State<Arc<AppState>>) -> Json<Value> {
    let agents = state.agents.read().await;
    let mut result = serde_json::Map::new();
    if let Some(map) = agents.as_object() {
        for (name, agent) in map {
            let last_seen = agent.get("lastSeen").cloned().unwrap_or(json!(null));
            let online = is_online(agent);
            let status = online_status(agent);
            result.insert(name.clone(), json!({
                "agent": name,
                "ts": last_seen,
                "status": status,
                "online": online,
                "decommissioned": agent.get("decommissioned").and_then(|v| v.as_bool()).unwrap_or(false),
                "lastSeen": last_seen,
            }));
        }
    }
    Json(Value::Object(result))
}

async fn get_agent_by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let agents = state.agents.read().await;
    match agents.as_object().and_then(|m| m.get(&name)) {
        Some(agent) => {
            let mut record = agent.clone();
            if let Some(obj) = record.as_object_mut() {
                obj.insert("online".into(), json!(is_online(agent)));
                obj.insert("onlineStatus".into(), json!(online_status(agent)));
            }
            Json(json!({ "ok": true, "agent": record })).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "Agent not found"}))).into_response(),
    }
}

/// GET /api/agents/:name/health — on-demand telemetry from last heartbeat
async fn get_agent_health(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let agents = state.agents.read().await;
    let agent_name = name.to_lowercase();
    match agents.as_object().and_then(|m| m.get(&agent_name)) {
        Some(agent) => {
            let telemetry_keys = [
                "gpu", "gpu_temp_c", "gpu_power_w", "gpu_util_pct",
                "vram_used_mb", "vram_total_mb",
                "unified_vram_used_mb", "unified_vram_free_mb", "unified_vram_total_mb",
                "ram", "ram_used_mb", "ram_avail_mb", "ram_total_mb",
                "ollama_status", "ollama_models", "ccc_version",
            ];
            let mut health = serde_json::Map::new();
            health.insert("agent".into(), json!(agent_name));
            health.insert("online".into(), json!(is_online(agent)));
            health.insert("lastSeen".into(), agent.get("lastSeen").cloned().unwrap_or(json!(null)));
            health.insert("host".into(), agent.get("host").cloned().unwrap_or(json!(null)));
            for key in &telemetry_keys {
                if let Some(val) = agent.get(*key) {
                    health.insert((*key).to_string(), val.clone());
                }
            }
            Json(json!({ "ok": true, "health": Value::Object(health) })).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "Agent not found"}))).into_response(),
    }
}

async fn agent_heartbeat(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let agent_name = name.to_lowercase();
    let now = chrono::Utc::now().to_rfc3339();

    let mut agents = state.agents.write().await;
    let agents_map = agents.as_object_mut().unwrap();

    let telemetry_keys = [
        "gpu", "gpu_temp_c", "gpu_power_w", "gpu_util_pct",
        "vram_used_mb", "vram_total_mb",
        "unified_vram_used_mb", "unified_vram_free_mb", "unified_vram_total_mb",
        "ram", "ram_used_mb", "ram_avail_mb", "ram_total_mb",
        "ollama_status", "ollama_models", "ccc_version",
        "ssh_user", "ssh_host", "ssh_port",
    ];

    if let Some(agent_obj) = agents_map.get_mut(&agent_name) {
        if let Some(obj) = agent_obj.as_object_mut() {
            obj.insert("lastSeen".into(), json!(now));
            for key in &telemetry_keys {
                if let Some(val) = body.get(*key) {
                    obj.insert((*key).to_string(), val.clone());
                }
            }
        }
    } else {
        let host = body.get("host").and_then(|h| h.as_str()).unwrap_or("unknown").to_string();
        let token = format!("acc-agent-{}-{}",agent_name, uuid::Uuid::new_v4().to_string().replace('-', ""));
        agents_map.insert(agent_name.clone(), json!({
            "name": agent_name,
            "host": host,
            "type": "full",
            "token": token,
            "registeredAt": now,
            "lastSeen": now,
            "capabilities": body.get("capabilities").cloned().unwrap_or(json!({})),
            "billing": {"claude_cli": "fixed", "inference_key": "metered", "gpu": "fixed"},
        }));
    }

    drop(agents);
    db_flush_agents(&state).await;

    Json(json!({ "ok": true })).into_response()
}

async fn register_agent(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let name = match body.get("name").and_then(|n| n.as_str()) {
        Some(n) => n.to_string(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "name required"}))).into_response(),
    };

    let mut agents = state.agents.write().await;
    let agents_map = agents.as_object_mut().unwrap();

    let existing_token = agents_map.get(&name)
        .and_then(|a| a.get("token"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let token = existing_token.unwrap_or_else(|| {
        format!("acc-agent-{}-{}", name, uuid::Uuid::new_v4().to_string().replace('-', ""))
    });

    let now = chrono::Utc::now().to_rfc3339();
    let existing = agents_map.get(&name).cloned().unwrap_or(json!({}));

    let caps_raw = body.get("capabilities").cloned().unwrap_or(json!({}));
    let existing_caps = existing.get("capabilities").cloned().unwrap_or(json!({}));

    // Support both array format (["openclaw", "claude"]) and legacy boolean-map format
    let capabilities_value = if caps_raw.is_array() {
        caps_raw.clone()
    } else {
        let caps = &caps_raw;
        json!({
            "claude_cli": caps.get("claude_cli").or_else(|| existing_caps.get("claude_cli")).and_then(|v| v.as_bool()).unwrap_or(true),
            "inference_key": caps.get("inference_key").or_else(|| existing_caps.get("inference_key")).and_then(|v| v.as_bool()).unwrap_or(true),
            "gpu": caps.get("gpu").or_else(|| existing_caps.get("gpu")).and_then(|v| v.as_bool()).unwrap_or(false),
            "tailscale": caps.get("tailscale").or_else(|| existing_caps.get("tailscale")).and_then(|v| v.as_bool()).unwrap_or(false),
            "tailscale_ip": caps.get("tailscale_ip").or_else(|| existing_caps.get("tailscale_ip")).cloned().unwrap_or(json!(null)),
            "vllm": caps.get("vllm").or_else(|| existing_caps.get("vllm")).and_then(|v| v.as_bool()).unwrap_or(false),
            "vllm_port": caps.get("vllm_port").or_else(|| existing_caps.get("vllm_port")).and_then(|v| v.as_u64()).unwrap_or(8080),
        })
    };

    let agent = json!({
        "name": name,
        "host": body.get("host").or_else(|| existing.get("host")).and_then(|h| h.as_str()).unwrap_or("unknown"),
        "type": body.get("type").or_else(|| existing.get("type")).and_then(|t| t.as_str()).unwrap_or("full"),
        "version": body.get("version").or_else(|| existing.get("version")).cloned().unwrap_or(json!(null)),
        "ccc_version": body.get("ccc_version").or_else(|| existing.get("ccc_version")).cloned().unwrap_or(json!(null)),
        "vllm_port": body.get("vllm_port").or_else(|| existing.get("vllm_port")).cloned().unwrap_or(json!(null)),
        "slack_id": body.get("slack_id").or_else(|| existing.get("slack_id")).cloned().unwrap_or(json!(null)),
        "token": token,
        "registeredAt": existing.get("registeredAt").cloned().unwrap_or(json!(now)),
        "lastSeen": json!(now),
        "capabilities": capabilities_value,
        "billing": {
            "claude_cli": "fixed",
            "inference_key": "metered",
            "gpu": "fixed",
        },
    });

    agents_map.insert(name.clone(), agent.clone());
    drop(agents);
    db_flush_agents(&state).await;

    (StatusCode::CREATED, Json(json!({"ok": true, "token": token, "agent": agent}))).into_response()
}

async fn post_agent(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    register_agent(State(state), Json(body)).await
}

async fn upsert_agent(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut agents = state.agents.write().await;
    let agents_map = agents.as_object_mut().unwrap();
    let now = chrono::Utc::now().to_rfc3339();

    if !agents_map.contains_key(&name) {
        let token = format!("acc-agent-{}-{}",name, uuid::Uuid::new_v4().to_string().replace('-', ""));
        agents_map.insert(name.clone(), json!({
            "name": name,
            "host": body.get("host").and_then(|h| h.as_str()).unwrap_or("unknown"),
            "type": body.get("type").and_then(|t| t.as_str()).unwrap_or("full"),
            "token": token,
            "registeredAt": now,
            "lastSeen": null,
            "capabilities": body.get("capabilities").cloned().unwrap_or(json!({})),
            "billing": {"claude_cli": "fixed", "inference_key": "metered", "gpu": "fixed"},
        }));
    } else {
        let agent = agents_map.get_mut(&name).unwrap().as_object_mut().unwrap();
        if let Some(h) = body.get("host").and_then(|h| h.as_str()) { agent.insert("host".into(), json!(h)); }
        if let Some(t) = body.get("type").and_then(|t| t.as_str()) { agent.insert("type".into(), json!(t)); }
        if let Some(caps) = body.get("capabilities") {
            let existing_caps = agent.entry("capabilities").or_insert(json!({}));
            if let (Some(ec), Some(nc)) = (existing_caps.as_object_mut(), caps.as_object()) {
                for (k, v) in nc { ec.insert(k.clone(), v.clone()); }
            }
        }
    }

    let token = agents_map[&name].get("token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let agent = agents_map[&name].clone();
    drop(agents);
    db_flush_agents(&state).await;

    Json(json!({"ok": true, "token": token, "agent": agent})).into_response()
}

async fn patch_agent(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    _headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut agents = state.agents.write().await;
    let agents_map = agents.as_object_mut().unwrap();

    if !agents_map.contains_key(&name) {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "Agent not found"}))).into_response();
    }

    let agent = agents_map.get_mut(&name).unwrap().as_object_mut().unwrap();
    let now = chrono::Utc::now().to_rfc3339();

    if let Some(h) = body.get("host").and_then(|h| h.as_str()) { agent.insert("host".into(), json!(h)); }
    if let Some(t) = body.get("type").and_then(|t| t.as_str()) { agent.insert("type".into(), json!(t)); }
    if let Some(caps) = body.get("capabilities").and_then(|c| c.as_object()) {
        let existing_caps = agent.entry("capabilities").or_insert(json!({}));
        if let Some(ec) = existing_caps.as_object_mut() {
            for (k, v) in caps { ec.insert(k.clone(), v.clone()); }
        }
    }
    if let Some(billing) = body.get("billing").and_then(|b| b.as_object()) {
        let existing_billing = agent.entry("billing").or_insert(json!({}));
        if let Some(eb) = existing_billing.as_object_mut() {
            for (k, v) in billing { eb.insert(k.clone(), v.clone()); }
        }
    }
    if let Some(status) = body.get("status").and_then(|s| s.as_str()) {
        if status == "decommissioned" {
            agent.insert("decommissioned".into(), json!(true));
            agent.insert("decommissionedAt".into(), json!(now));
            agent.insert("onlineStatus".into(), json!("decommissioned"));
        } else if status == "active" {
            agent.remove("decommissioned");
            agent.remove("decommissionedAt");
            agent.insert("onlineStatus".into(), json!("unknown"));
        }
    }

    let updated = agents_map[&name].clone();
    drop(agents);
    db_flush_agents(&state).await;

    Json(json!({"ok": true, "agent": updated})).into_response()
}

async fn delete_agent(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }
    let mut agents = state.agents.write().await;
    let agents_map = agents.as_object_mut().unwrap();
    if agents_map.remove(&name).is_none() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "Agent not found"}))).into_response();
    }
    drop(agents);
    db_flush_agents(&state).await;
    Json(json!({"ok": true, "name": name, "deleted": true})).into_response()
}

/// PUT /api/agents/:name/capabilities
/// Body: {"capabilities": ["bash", "read_file", "llm:claude-opus-4-7"]}
/// Stores tool_capabilities[] separately from the legacy capabilities{} object.
/// Called by agents at startup to advertise their live tool set.
async fn register_tool_capabilities(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error":"Unauthorized"}))).into_response();
    }
    let caps: Vec<String> = body["capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let mut agents = state.agents.write().await;
    let agents_map = match agents.as_object_mut()
        .ok_or(())
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"agents not object"}))))
    {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let agent = agents_map.entry(name.clone()).or_insert(json!({}));
    agent["tool_capabilities"] = serde_json::json!(caps);
    agent["lastSeen"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    drop(agents);
    state::db_flush_agents(&state).await;

    Json(json!({"ok":true,"name":name,"capabilities":caps})).into_response()
}

async fn post_heartbeat(
    State(state): State<Arc<AppState>>,
    Path(agent_name): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let agent = agent_name.to_lowercase();
    let now = chrono::Utc::now().to_rfc3339();

    // Telemetry fields we pass through from the heartbeat body into the agent record.
    // This lets agents like Natasha surface GPU stats, ollama status, etc. in the dashboard.
    let telemetry_keys = [
        "gpu", "gpu_temp_c", "gpu_power_w", "gpu_util_pct",
        "vram_used_mb", "vram_total_mb",
        "unified_vram_used_mb", "unified_vram_free_mb", "unified_vram_total_mb",
        "ram", "ram_used_mb", "ram_avail_mb", "ram_total_mb",
        "ollama_status", "ollama_models", "ccc_version",
        "ssh_user", "ssh_host", "ssh_port",
    ];

    let mut agents = state.agents.write().await;
    if let Some(agent_obj) = agents.as_object_mut().and_then(|m| m.get_mut(&agent)) {
        if let Some(obj) = agent_obj.as_object_mut() {
            obj.insert("lastSeen".into(), json!(now));
            // Update host if agent reports a better value (self-heals stale registry entries)
            if let Some(h) = body.get("host").and_then(|h| h.as_str()) {
                obj.insert("host".into(), json!(h));
            }
            // Merge telemetry fields if present in the heartbeat payload
            for key in &telemetry_keys {
                if let Some(val) = body.get(*key) {
                    obj.insert((*key).to_string(), val.clone());
                }
            }
        }
    }
    drop(agents);
    db_flush_agents(&state).await;

    let q = state.queue.read().await;
    let pending_work: Vec<Value> = q.items.iter()
        .filter(|i| {
            i.get("status").and_then(|s| s.as_str()) == Some("pending")
            && (i.get("assignee").and_then(|a| a.as_str()) == Some(&agent)
                || i.get("assignee").and_then(|a| a.as_str()) == Some("all"))
        })
        .take(3)
        .map(|i| json!({
            "id": i.get("id"),
            "title": i.get("title"),
            "priority": i.get("priority"),
            "description": i.get("description"),
        }))
        .collect();

    Json(json!({"ok": true, "pendingWork": pending_work})).into_response()
}
