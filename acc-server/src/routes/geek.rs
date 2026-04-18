use crate::AppState;
/// /api/geek — GeekView topology + SSE stream (Rust port of routes/agents.mjs geek section)
use axum::{
    extract::State,
    response::{sse::Event, IntoResponse, Json, Sse},
    routing::get,
    Router,
};
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/geek/topology", get(topology))
        .route("/api/geek/stream", get(geek_stream))
        .route("/api/mesh", get(topology)) // alias
}

const STALE_MS: i64 = 5 * 60 * 1000;
const OFFLINE_MS: i64 = 30 * 60 * 1000;

async fn topology(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let agents = state.agents.read().await;
    let now = chrono::Utc::now().timestamp_millis();

    // Static node list (topology is defined here; heartbeat data is overlaid)
    let raw_nodes: Vec<Value> = vec![
        json!({"id":"rocky",         "label":"Rocky",           "type":"agent",          "host":"do-host1",   "chips":["CCC API :8789","TokenHub :8090","AgentBus hub","Tailscale proxy"]}),
        json!({"id":"bullwinkle",    "label":"Bullwinkle",      "type":"agent",          "host":"puck",       "chips":["OpenClaw :18789","launchd crons"]}),
        json!({"id":"natasha",       "label":"Natasha",         "type":"agent",          "host":"sparky",     "chips":["OpenClaw :18789","Qdrant :6333","CUDA/RTX","Ollama :11434"]}),
        json!({"id":"boris",         "label":"Boris",           "type":"agent",          "host":"l40-sweden", "chips":["OpenClaw gateway","4x L40","Nemotron-120B"]}),
        json!({"id":"peabody",       "label":"Peabody",         "type":"agent",          "host":"l40-sweden", "chips":["OpenClaw gateway","4x L40","Nemotron-120B"]}),
        json!({"id":"sherman",       "label":"Sherman",         "type":"agent",          "host":"l40-sweden", "chips":["OpenClaw gateway","4x L40","Nemotron-120B"]}),
        json!({"id":"snidely",       "label":"Snidely",         "type":"agent",          "host":"l40-sweden", "chips":["OpenClaw gateway","4x L40","Nemotron-120B"]}),
        json!({"id":"dudley",        "label":"Dudley",          "type":"agent",          "host":"l40-sweden", "chips":["OpenClaw gateway","4x L40","Nemotron-120B"]}),
        json!({"id":"qdrant",        "label":"Qdrant",          "type":"shared-service", "host":"do-host1",   "port":6333}),
        json!({"id":"accfs",         "label":"AccFS (Samba)",   "type":"shared-service", "host":"do-host1",   "port":445}),
        json!({"id":"searxng",       "label":"SearXNG",         "type":"shared-service", "host":"do-host1",   "port":8888}),
        json!({"id":"tokenhub",      "label":"TokenHub",        "type":"shared-service", "host":"do-host1",   "port":8090}),
        json!({"id":"nvidia-gateway","label":"NVIDIA Gateway",  "type":"external",       "url":"inference-api.nvidia.com"}),
        json!({"id":"github",        "label":"GitHub",          "type":"external",       "url":"api.github.com"}),
        json!({"id":"slack-omgjkh",  "label":"Slack (omgjkh)",  "type":"external",       "url":"omgjkh.slack.com"}),
        json!({"id":"telegram",      "label":"Telegram",        "type":"external",       "url":"api.telegram.org"}),
        json!({"id":"agentbus",       "label":"AgentBus",     "type":"bus",            "host":"do-host1"}),
    ];

    let edges: Vec<Value> = vec![
        json!({"from":"bullwinkle","to":"rocky",          "type":"persistent","protocol":"heartbeat/HTTP"}),
        json!({"from":"natasha",   "to":"rocky",          "type":"persistent","protocol":"heartbeat/HTTP"}),
        json!({"from":"boris",     "to":"rocky",          "type":"persistent","protocol":"heartbeat/HTTP"}),
        json!({"from":"peabody",   "to":"rocky",          "type":"persistent","protocol":"heartbeat/HTTP"}),
        json!({"from":"sherman",   "to":"rocky",          "type":"persistent","protocol":"heartbeat/HTTP"}),
        json!({"from":"snidely",   "to":"rocky",          "type":"persistent","protocol":"heartbeat/HTTP"}),
        json!({"from":"dudley",    "to":"rocky",          "type":"persistent","protocol":"heartbeat/HTTP"}),
        json!({"from":"rocky",     "to":"qdrant",         "type":"on-demand", "protocol":"gRPC"}),
        json!({"from":"rocky",     "to":"accfs",          "type":"persistent","protocol":"SMB/local"}),
        json!({"from":"rocky",     "to":"agentbus",        "type":"persistent","protocol":"JSONL/fanout"}),
        json!({"from":"rocky",     "to":"tokenhub",       "type":"persistent","protocol":"HTTP/OpenAI"}),
        json!({"from":"rocky",     "to":"nvidia-gateway", "type":"on-demand", "protocol":"HTTPS/OpenAI"}),
        json!({"from":"rocky",     "to":"github",         "type":"on-demand", "protocol":"HTTPS/REST"}),
        json!({"from":"rocky",     "to":"slack-omgjkh",   "type":"persistent","protocol":"Socket Mode"}),
        json!({"from":"rocky",     "to":"telegram",       "type":"on-demand", "protocol":"HTTPS/Bot API"}),
    ];

    // Overlay heartbeat status on agent nodes
    let nodes: Vec<Value> = raw_nodes
        .into_iter()
        .map(|mut n| {
            if n.get("type").and_then(|v| v.as_str()) != Some("agent") {
                return n;
            }
            let agent_id = n
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let agent_data = agents
                .as_object()
                .and_then(|m| m.get(&agent_id))
                .cloned()
                .unwrap_or(json!(null));

            let last_seen = agent_data
                .get("lastSeen")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let status = last_seen
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| {
                    let age = now - dt.timestamp_millis();
                    if age < STALE_MS {
                        "online"
                    } else if age < OFFLINE_MS {
                        "stale"
                    } else {
                        "offline"
                    }
                })
                .unwrap_or("offline");

            let obj = n.as_object_mut().unwrap();
            obj.insert("status".to_string(), json!(status));
            obj.insert("lastSeen".to_string(), json!(last_seen));
            n
        })
        .collect();

    // Build heartbeat summary
    let heartbeat_summary: Vec<Value> = agents
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(name, agent)| {
                    let ts = agent.get("lastSeen").cloned().unwrap_or(json!(null));
                    json!({"agent": name, "ts": ts, "status": "online"})
                })
                .collect()
        })
        .unwrap_or_default();

    drop(agents);

    Json(json!({
        "nodes": nodes,
        "edges": edges,
        "heartbeatSummary": heartbeat_summary,
        "busMessages": [],  // TODO: read from bus log when SOA-007 wires bus JSONL
    }))
}

// ── GET /api/geek/stream — SSE ────────────────────────────────────────────

async fn geek_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let mut bus_rx = state.bus_tx.subscribe();

    let stream = stream::unfold((bus_rx, true), move |(mut rx, first)| {
        async move {
            if first {
                let event = Event::default().data(json!({"type":"connected"}).to_string());
                return Some((Ok::<Event, Infallible>(event), (rx, false)));
            }
            // Forward bus events tagged as geek updates, or keepalive every 15s
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Ok(data) => {
                                let event = Event::default().data(data);
                                return Some((Ok(event), (rx, false)));
                            }
                            Err(_) => {
                                tokio::time::sleep(Duration::from_secs(15)).await;
                                let event = Event::default().comment("keepalive");
                                return Some((Ok(event), (rx, false)));
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(15)) => {
                        let event = Event::default().comment("keepalive");
                        return Some((Ok(event), (rx, false)));
                    }
                }
            }
        }
    });

    Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)))
}
