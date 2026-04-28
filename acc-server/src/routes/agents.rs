use crate::state;
use crate::state::db_flush_agents;
use crate::AppState;
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

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/agents", get(get_agents).post(post_agent))
        .route("/api/agents/names", get(get_agent_names))
        .route("/api/agents/register", post(register_agent))
        .route(
            "/api/agents/:name",
            get(get_agent_by_name)
                .post(upsert_agent)
                .patch(patch_agent)
                .delete(delete_agent),
        )
        .route("/api/agents/:name/heartbeat", post(agent_heartbeat))
        .route("/api/agents/:name/health", get(get_agent_health))
        .route(
            "/api/agents/:name/capabilities",
            put(register_tool_capabilities),
        )
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
    if agent
        .get("decommissioned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return "decommissioned";
    }
    if is_online(agent) {
        "online"
    } else {
        "offline"
    }
}

const KNOWN_EXECUTORS: &[&str] = &[
    "claude_cli",
    "codex_cli",
    "cursor_cli",
    "opencode",
    "inference_key",
    "gpu",
    "vllm",
    "ollama",
    "hermes",
];

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_capabilities_map(
    raw: Option<&Value>,
    existing: Option<&Value>,
) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    for source in [existing, raw] {
        match source {
            Some(Value::Object(obj)) => {
                for (k, v) in obj {
                    map.insert(k.clone(), v.clone());
                }
            }
            Some(Value::Array(arr)) => {
                for cap in arr.iter().filter_map(|v| v.as_str()) {
                    map.insert(cap.to_string(), json!(true));
                }
            }
            _ => {}
        }
    }
    map
}

fn merge_string_lists(primary: Vec<String>, secondary: Vec<String>) -> Vec<String> {
    let mut merged = Vec::new();
    for value in primary.into_iter().chain(secondary.into_iter()) {
        if !value.is_empty() && !merged.iter().any(|v| v == &value) {
            merged.push(value);
        }
    }
    merged
}

fn derive_executors(
    body: &Value,
    existing: &Value,
    caps: &serde_json::Map<String, Value>,
    tool_capabilities: &[String],
) -> Vec<Value> {
    if let Some(executors) = body.get("executors").and_then(|v| v.as_array()) {
        return executors.iter().map(normalize_executor_entry).collect();
    }
    if let Some(executors) = existing.get("executors").and_then(|v| v.as_array()) {
        return executors.iter().map(normalize_executor_entry).collect();
    }

    let mut names: Vec<String> = caps
        .iter()
        .filter_map(|(name, val)| val.as_bool().filter(|b| *b).map(|_| name.clone()))
        .filter(|name| KNOWN_EXECUTORS.contains(&name.as_str()))
        .collect();
    for cap in tool_capabilities {
        if KNOWN_EXECUTORS.contains(&cap.as_str()) && !names.iter().any(|n| n == cap) {
            names.push(cap.clone());
        }
    }
    names.sort();
    names.into_iter()
        .map(|executor| json!({"executor": executor, "type": executor, "ready": true, "auth_state": "unknown"}))
        .collect()
}

fn normalize_executor_entry(entry: &Value) -> Value {
    let mut normalized = entry.as_object().cloned().unwrap_or_default();
    let executor = normalized
        .get("executor")
        .and_then(|v| v.as_str())
        .or_else(|| normalized.get("type").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    if !executor.is_empty() {
        normalized.insert("executor".into(), json!(executor));
        normalized.insert("type".into(), json!(executor));
    }
    Value::Object(normalized)
}

fn normalize_capacity(body: &Value, existing: &Value) -> Value {
    let mut capacity = existing
        .get("capacity")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    if let Some(obj) = body.get("capacity").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            capacity.insert(k.clone(), v.clone());
        }
    }
    for key in [
        "tasks_in_flight",
        "estimated_free_slots",
        "free_session_slots",
        "max_sessions",
        "session_spawn_denied_reason",
    ] {
        if let Some(val) = body
            .get(key)
            .cloned()
            .or_else(|| existing.get(key).cloned())
        {
            capacity.insert(key.to_string(), val);
        }
    }
    if capacity.is_empty() {
        Value::Null
    } else {
        Value::Object(capacity)
    }
}

fn refresh_canonical_agent_shape(agent: &mut Value, body: &Value) {
    let existing = agent.clone();
    let caps = normalize_capabilities_map(body.get("capabilities"), existing.get("capabilities"));
    let tool_caps = merge_string_lists(
        string_list(body.get("tool_capabilities")),
        merge_string_lists(
            string_list(existing.get("tool_capabilities")),
            string_list(body.get("capabilities")),
        ),
    );
    let executors = derive_executors(body, &existing, &caps, &tool_caps);
    let sessions = body
        .get("sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .or_else(|| existing.get("sessions").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let capacity = normalize_capacity(body, &existing);

    agent["capabilities"] = Value::Object(caps);
    agent["tool_capabilities"] = json!(tool_caps);
    agent["executors"] = json!(executors);
    agent["sessions"] = json!(sessions);
    if capacity.is_null() {
        if let Some(obj) = agent.as_object_mut() {
            obj.remove("capacity");
        }
    } else {
        agent["capacity"] = capacity;
    }
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
        Some(map) => map
            .values()
            .filter_map(|a| {
                if online_only && !is_online(a) {
                    return None;
                }
                let mut record = a.clone();
                if let Some(obj) = record.as_object_mut() {
                    obj.insert("online".into(), json!(is_online(a)));
                    obj.insert("onlineStatus".into(), json!(online_status(a)));
                }
                Some(record)
            })
            .collect(),
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
        Some(map) => map
            .iter()
            .filter_map(|(name, a)| {
                if online_only && !is_online(a) {
                    return None;
                }
                if online_status(a) == "decommissioned" {
                    return None;
                }
                Some(name.as_str())
            })
            .collect(),
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
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Agent not found"})),
        )
            .into_response(),
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
                "gpu",
                "gpu_temp_c",
                "gpu_power_w",
                "gpu_util_pct",
                "vram_used_mb",
                "vram_total_mb",
                "unified_vram_used_mb",
                "unified_vram_free_mb",
                "unified_vram_total_mb",
                "ram",
                "ram_used_mb",
                "ram_avail_mb",
                "ram_total_mb",
                "ollama_status",
                "ollama_models",
                "ccc_version",
            ];
            let mut health = serde_json::Map::new();
            health.insert("agent".into(), json!(agent_name));
            health.insert("online".into(), json!(is_online(agent)));
            health.insert(
                "lastSeen".into(),
                agent.get("lastSeen").cloned().unwrap_or(json!(null)),
            );
            health.insert(
                "host".into(),
                agent.get("host").cloned().unwrap_or(json!(null)),
            );
            for key in &telemetry_keys {
                if let Some(val) = agent.get(*key) {
                    health.insert((*key).to_string(), val.clone());
                }
            }
            Json(json!({ "ok": true, "health": Value::Object(health) })).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "Agent not found"})),
        )
            .into_response(),
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
        "gpu",
        "gpu_temp_c",
        "gpu_power_w",
        "gpu_util_pct",
        "vram_used_mb",
        "vram_total_mb",
        "unified_vram_used_mb",
        "unified_vram_free_mb",
        "unified_vram_total_mb",
        "ram",
        "ram_used_mb",
        "ram_avail_mb",
        "ram_total_mb",
        "ollama_status",
        "ollama_models",
        "ccc_version",
        "ssh_user",
        "ssh_host",
        "ssh_port",
        "tasks_in_flight",
        "estimated_free_slots",
        "free_session_slots",
        "max_sessions",
        "session_spawn_denied_reason",
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
        refresh_canonical_agent_shape(agent_obj, &body);
    } else {
        let host = body
            .get("host")
            .and_then(|h| h.as_str())
            .unwrap_or("unknown")
            .to_string();
        let token = format!(
            "acc-agent-{}-{}",
            agent_name,
            uuid::Uuid::new_v4().to_string().replace('-', "")
        );
        let mut record = json!({
            "name": agent_name,
            "host": host,
            "type": "full",
            "token": token,
            "registeredAt": now,
            "lastSeen": now,
            "capabilities": body.get("capabilities").cloned().unwrap_or(json!({})),
            "billing": {"claude_cli": "fixed", "inference_key": "metered", "gpu": "fixed"},
        });
        refresh_canonical_agent_shape(&mut record, &body);
        agents_map.insert(agent_name.clone(), record);
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
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "name required"})),
            )
                .into_response()
        }
    };

    let mut agents = state.agents.write().await;
    let agents_map = agents.as_object_mut().unwrap();

    let existing_token = agents_map
        .get(&name)
        .and_then(|a| a.get("token"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let token = existing_token.unwrap_or_else(|| {
        format!(
            "acc-agent-{}-{}",
            name,
            uuid::Uuid::new_v4().to_string().replace('-', "")
        )
    });

    let now = chrono::Utc::now().to_rfc3339();
    let existing = agents_map.get(&name).cloned().unwrap_or(json!({}));

    let mut agent = json!({
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
        "capabilities": body.get("capabilities").or_else(|| existing.get("capabilities")).cloned().unwrap_or(json!({})),
        "billing": {
            "claude_cli": "fixed",
            "inference_key": "metered",
            "gpu": "fixed",
        },
    });
    refresh_canonical_agent_shape(&mut agent, &body);

    agents_map.insert(name.clone(), agent.clone());
    drop(agents);
    db_flush_agents(&state).await;

    (
        StatusCode::CREATED,
        Json(json!({"ok": true, "token": token, "agent": agent})),
    )
        .into_response()
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
        let token = format!(
            "acc-agent-{}-{}",
            name,
            uuid::Uuid::new_v4().to_string().replace('-', "")
        );
        let mut record = json!({
            "name": name,
            "host": body.get("host").and_then(|h| h.as_str()).unwrap_or("unknown"),
            "type": body.get("type").and_then(|t| t.as_str()).unwrap_or("full"),
            "token": token,
            "registeredAt": now,
            "lastSeen": null,
            "capabilities": body.get("capabilities").cloned().unwrap_or(json!({})),
            "billing": {"claude_cli": "fixed", "inference_key": "metered", "gpu": "fixed"},
        });
        refresh_canonical_agent_shape(&mut record, &body);
        agents_map.insert(name.clone(), record);
    } else {
        let agent = agents_map.get_mut(&name).unwrap().as_object_mut().unwrap();
        if let Some(h) = body.get("host").and_then(|h| h.as_str()) {
            agent.insert("host".into(), json!(h));
        }
        if let Some(t) = body.get("type").and_then(|t| t.as_str()) {
            agent.insert("type".into(), json!(t));
        }
        if let Some(caps) = body.get("capabilities") {
            agent.insert("capabilities".into(), caps.clone());
        }
        let value = agents_map.get_mut(&name).unwrap();
        refresh_canonical_agent_shape(value, &body);
    }

    let token = agents_map[&name]
        .get("token")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
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
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Agent not found"})),
        )
            .into_response();
    }

    let agent = agents_map.get_mut(&name).unwrap().as_object_mut().unwrap();
    let now = chrono::Utc::now().to_rfc3339();

    if let Some(h) = body.get("host").and_then(|h| h.as_str()) {
        agent.insert("host".into(), json!(h));
    }
    if let Some(t) = body.get("type").and_then(|t| t.as_str()) {
        agent.insert("type".into(), json!(t));
    }
    if let Some(caps) = body.get("capabilities") {
        agent.insert("capabilities".into(), caps.clone());
    }
    if let Some(billing) = body.get("billing").and_then(|b| b.as_object()) {
        let existing_billing = agent.entry("billing").or_insert(json!({}));
        if let Some(eb) = existing_billing.as_object_mut() {
            for (k, v) in billing {
                eb.insert(k.clone(), v.clone());
            }
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
    let updated_value = agents_map.get_mut(&name).unwrap();
    refresh_canonical_agent_shape(updated_value, &body);

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
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    let mut agents = state.agents.write().await;
    let agents_map = agents.as_object_mut().unwrap();
    if agents_map.remove(&name).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Agent not found"})),
        )
            .into_response();
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        )
            .into_response();
    }
    let caps: Vec<String> = body["capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut agents = state.agents.write().await;
    let agents_map = match agents.as_object_mut().ok_or(()).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"agents not object"})),
        )
    }) {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let agent = agents_map.entry(name.clone()).or_insert(json!({}));
    agent["tool_capabilities"] = serde_json::json!(caps);
    agent["lastSeen"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    refresh_canonical_agent_shape(agent, &json!({"tool_capabilities": caps}));
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
        "gpu",
        "gpu_temp_c",
        "gpu_power_w",
        "gpu_util_pct",
        "vram_used_mb",
        "vram_total_mb",
        "unified_vram_used_mb",
        "unified_vram_free_mb",
        "unified_vram_total_mb",
        "ram",
        "ram_used_mb",
        "ram_avail_mb",
        "ram_total_mb",
        "ollama_status",
        "ollama_models",
        "ccc_version",
        "ssh_user",
        "ssh_host",
        "ssh_port",
        "tasks_in_flight",
        "estimated_free_slots",
        "free_session_slots",
        "max_sessions",
        "session_spawn_denied_reason",
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
        if let Some(agent_obj) = agents.as_object_mut().and_then(|m| m.get_mut(&agent)) {
            refresh_canonical_agent_shape(agent_obj, &body);
        }
    }
    drop(agents);
    db_flush_agents(&state).await;

    let q = state.queue.read().await;
    let pending_work: Vec<Value> = q
        .items
        .iter()
        .filter(|i| {
            i.get("status").and_then(|s| s.as_str()) == Some("pending")
                && (i.get("assignee").and_then(|a| a.as_str()) == Some(&agent)
                    || i.get("assignee").and_then(|a| a.as_str()) == Some("all"))
        })
        .take(3)
        .map(|i| {
            json!({
                "id": i.get("id"),
                "title": i.get("title"),
                "priority": i.get("priority"),
                "description": i.get("description"),
            })
        })
        .collect();

    Json(json!({"ok": true, "pendingWork": pending_work})).into_response()
}
