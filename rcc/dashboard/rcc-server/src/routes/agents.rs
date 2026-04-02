use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::sync::Arc;
use crate::AppState;
use crate::state::flush_agents;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/agents", get(get_agents).post(post_agent))
        .route("/api/agents/register", post(register_agent))
        .route("/api/agents/:name", post(upsert_agent).patch(patch_agent))
        .route("/api/heartbeat/:agent", post(post_heartbeat))
        .route("/api/heartbeats", get(get_heartbeats))
}

async fn get_agents(State(state): State<Arc<AppState>>) -> Json<Value> {
    let agents = state.agents.read().await;
    let result: Vec<Value> = match agents.as_object() {
        Some(map) => map.values().cloned().collect(),
        None => vec![],
    };
    Json(json!(result))
}

async fn get_heartbeats(State(state): State<Arc<AppState>>) -> Json<Value> {
    let agents = state.agents.read().await;
    let mut result = serde_json::Map::new();
    if let Some(map) = agents.as_object() {
        for (name, agent) in map {
            let last_seen = agent.get("lastSeen").cloned().unwrap_or(json!(null));
            let status = agent.get("onlineStatus").and_then(|s| s.as_str()).unwrap_or("unknown");
            let online = if let Some(ts_str) = last_seen.as_str() {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts_str) {
                    let age = chrono::Utc::now().signed_duration_since(dt.with_timezone(&chrono::Utc));
                    age.num_minutes() < 10
                } else { false }
            } else { false };
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
        format!("rcc-agent-{}-{}", name, uuid::Uuid::new_v4().to_string().replace('-', ""))
    });

    let now = chrono::Utc::now().to_rfc3339();
    let existing = agents_map.get(&name).cloned().unwrap_or(json!({}));

    let caps = body.get("capabilities").cloned().unwrap_or(json!({}));
    let existing_caps = existing.get("capabilities").cloned().unwrap_or(json!({}));

    let agent = json!({
        "name": name,
        "host": body.get("host").or_else(|| existing.get("host")).and_then(|h| h.as_str()).unwrap_or("unknown"),
        "type": body.get("type").or_else(|| existing.get("type")).and_then(|t| t.as_str()).unwrap_or("full"),
        "token": token,
        "registeredAt": existing.get("registeredAt").cloned().unwrap_or(json!(now)),
        "lastSeen": existing.get("lastSeen").cloned().unwrap_or(json!(null)),
        "capabilities": {
            "claude_cli": caps.get("claude_cli").or_else(|| existing_caps.get("claude_cli")).and_then(|v| v.as_bool()).unwrap_or(true),
            "inference_key": caps.get("inference_key").or_else(|| existing_caps.get("inference_key")).and_then(|v| v.as_bool()).unwrap_or(true),
            "gpu": caps.get("gpu").or_else(|| existing_caps.get("gpu")).and_then(|v| v.as_bool()).unwrap_or(false),
            "tailscale": caps.get("tailscale").or_else(|| existing_caps.get("tailscale")).and_then(|v| v.as_bool()).unwrap_or(false),
            "tailscale_ip": caps.get("tailscale_ip").or_else(|| existing_caps.get("tailscale_ip")).cloned().unwrap_or(json!(null)),
            "vllm": caps.get("vllm").or_else(|| existing_caps.get("vllm")).and_then(|v| v.as_bool()).unwrap_or(false),
            "vllm_port": caps.get("vllm_port").or_else(|| existing_caps.get("vllm_port")).and_then(|v| v.as_u64()).unwrap_or(8080),
        },
        "billing": {
            "claude_cli": "fixed",
            "inference_key": "metered",
            "gpu": "fixed",
        },
    });

    agents_map.insert(name.clone(), agent.clone());
    drop(agents);
    flush_agents(&state).await;

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
        let token = format!("rcc-agent-{}-{}", name, uuid::Uuid::new_v4().to_string().replace('-', ""));
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
    flush_agents(&state).await;

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
    flush_agents(&state).await;

    Json(json!({"ok": true, "agent": updated})).into_response()
}

async fn post_heartbeat(
    State(state): State<Arc<AppState>>,
    Path(agent_name): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let agent = agent_name.to_lowercase();
    let now = chrono::Utc::now().to_rfc3339();

    let mut agents = state.agents.write().await;
    if let Some(agent_obj) = agents.as_object_mut().and_then(|m| m.get_mut(&agent)) {
        if let Some(obj) = agent_obj.as_object_mut() {
            obj.insert("lastSeen".into(), json!(now));
            obj.insert("onlineStatus".into(), json!("online"));
        }
    }
    drop(agents);
    flush_agents(&state).await;

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
