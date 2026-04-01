/// Shared data context — single source of truth for queue + heartbeat data.
///
/// Provide at App level with `provide_context(DashboardContext::new())`.
/// Consume in child components with `use_context::<DashboardContext>()`.
///
/// This eliminates the FOUC / layout-shift caused by each component
/// independently fetching the same two endpoints and resolving at
/// slightly different wall-clock times.
use leptos::*;

use crate::types::{AgentList, HeartbeatMap, QueueResponse};

async fn fetch_queue() -> QueueResponse {
    let Ok(resp) = gloo_net::http::Request::get("/api/queue").send().await else {
        return QueueResponse::default();
    };
    // Try typed deserialization first.
    // If it fails (e.g., unexpected fields in 1000+ completed items),
    // fall back to raw text + serde_json with lossy item parsing.
    match resp.text().await {
        Ok(text) => {
            // Try full parse
            if let Ok(q) = serde_json::from_str::<QueueResponse>(&text) {
                return q;
            }
            // Full parse failed — try parsing items individually
            // (completed array is huge and may contain non-standard fields)
            if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&text) {
                let items = raw.get("items")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| serde_json::from_value::<crate::types::QueueItem>(v.clone()).ok())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let completed = raw.get("completed")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| serde_json::from_value::<crate::types::QueueItem>(v.clone()).ok())
                            .collect::<Vec<_>>()
                    });
                return QueueResponse { items, completed };
            }
            QueueResponse::default()
        }
        Err(_) => QueueResponse::default(),
    }
}

async fn fetch_heartbeats() -> HeartbeatMap {
    let Ok(resp) = gloo_net::http::Request::get("/api/heartbeats").send().await else {
        return HeartbeatMap::default();
    };
    resp.json::<HeartbeatMap>().await.unwrap_or_default()
}

async fn fetch_agents() -> AgentList {
    let Ok(resp) = gloo_net::http::Request::get("/api/agents").send().await else {
        return vec![];
    };
    resp.json::<AgentList>().await.unwrap_or_default()
}

#[derive(Clone)]
pub struct DashboardContext {
    /// Tick signal — increment to trigger a refresh of all shared resources.
    #[allow(dead_code)]
    pub tick: ReadSignal<u32>,
    #[allow(dead_code)]
    pub set_tick: WriteSignal<u32>,

    pub queue: Resource<u32, QueueResponse>,
    pub heartbeats: Resource<u32, HeartbeatMap>,
    pub agents: Resource<u32, AgentList>,
}

impl DashboardContext {
    pub fn new() -> Self {
        let (tick, set_tick) = create_signal(0u32);

        // Single polling loop: 30s cadence for all shared data.
        leptos::spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(30_000).await;
                set_tick.update(|t| *t = t.wrapping_add(1));
            }
        });

        let queue = create_resource(move || tick.get(), |_| fetch_queue());
        let heartbeats = create_resource(move || tick.get(), |_| fetch_heartbeats());
        let agents = create_resource(move || tick.get(), |_| fetch_agents());

        Self { tick, set_tick, queue, heartbeats, agents }
    }
}
