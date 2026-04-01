use gloo_net::http::Request;
use crate::types::{QueueItem, HeartbeatMap, BusMessage, Project, CalEvent, Provider};

pub const AUTH_TOKEN: &str = env!("RCC_AUTH_TOKEN", "Set RCC_AUTH_TOKEN at build time");

fn auth_header() -> String {
    format!("Bearer {}", AUTH_TOKEN)
}

// ── Queue ─────────────────────────────────────────────────────────────────────

pub async fn fetch_queue() -> Result<Vec<QueueItem>, String> {
    let resp = Request::get("/api/queue")
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<Vec<QueueItem>>()
        .await
        .map_err(|e| e.to_string())
}

pub async fn patch_item(id: &str, patch: serde_json::Value) -> Result<QueueItem, String> {
    let resp = Request::patch(&format!("/api/item/{}", id))
        .header("Authorization", &auth_header())
        .header("Content-Type", "application/json")
        .body(patch.to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<QueueItem>().await.map_err(|e| e.to_string())
}

pub async fn upvote_item(id: &str) -> Result<(), String> {
    let resp = Request::post(&format!("/api/upvote/{}", id))
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(())
}

pub async fn add_comment(id: &str, text: &str, author: &str) -> Result<(), String> {
    let body = serde_json::json!({ "text": text, "author": author });
    let resp = Request::post(&format!("/api/item/{}/comment", id))
        .header("Authorization", &auth_header())
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(())
}

// ── Heartbeats ────────────────────────────────────────────────────────────────

pub async fn fetch_heartbeats() -> Result<HeartbeatMap, String> {
    let resp = Request::get("/api/heartbeats")
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<HeartbeatMap>()
        .await
        .map_err(|e| e.to_string())
}

// ── Bus messages ──────────────────────────────────────────────────────────────

pub async fn fetch_bus_messages(limit: u32) -> Result<Vec<BusMessage>, String> {
    let resp = Request::get(&format!("/bus/messages?limit={}", limit))
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<Vec<BusMessage>>()
        .await
        .map_err(|e| e.to_string())
}

pub async fn send_bus_message(from: &str, to: &str, msg_type: &str, body: &str) -> Result<(), String> {
    let payload = serde_json::json!({
        "from": from,
        "to": to,
        "type": msg_type,
        "body": body,
        "mime": "text/plain"
    });

    let resp = Request::post("/bus/send")
        .header("Authorization", &auth_header())
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(())
}

// ── Projects ──────────────────────────────────────────────────────────────────

pub async fn fetch_projects() -> Result<Vec<Project>, String> {
    let resp = Request::get("/api/projects")
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<Vec<Project>>()
        .await
        .map_err(|e| e.to_string())
}

// ── Providers ─────────────────────────────────────────────────────────────────

pub async fn fetch_providers() -> Result<Vec<Provider>, String> {
    let resp = Request::get("/api/providers")
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<Vec<Provider>>()
        .await
        .map_err(|e| e.to_string())
}

// ── WASM profiler snapshot ────────────────────────────────────────────────────

pub async fn fetch_wasm_profiles() -> Result<crate::types::ProfileSnapshot, String> {
    let resp = Request::get("/api/agentos/wasm-profiles")
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<crate::types::ProfileSnapshot>()
        .await
        .map_err(|e| e.to_string())
}

// ── Cap audit events ──────────────────────────────────────────────────────────

pub async fn fetch_cap_events(limit: u32, slot: Option<u32>, event_type: Option<&str>) -> Result<crate::types::CapEventsResponse, String> {
    let mut url = format!("/api/agentos/cap-events?limit={}", limit);
    if let Some(s) = slot       { url.push_str(&format!("&slot={}", s)); }
    if let Some(t) = event_type { url.push_str(&format!("&type={}", t)); }

    let resp = Request::get(&url)
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<crate::types::CapEventsResponse>()
        .await
        .map_err(|e| e.to_string())
}

// ── Calendar ──────────────────────────────────────────────────────────────────

pub async fn fetch_calendar() -> Result<Vec<CalEvent>, String> {
    let resp = Request::get("/api/calendar")
        .header("Authorization", &auth_header())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    resp.json::<Vec<CalEvent>>()
        .await
        .map_err(|e| e.to_string())
}
