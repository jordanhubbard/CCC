use crate::state::flush_queue;
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

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/queue", get(get_queue).post(post_queue))
        .route("/api/queue/stale", get(get_stale))
        .route("/api/queue/claimed", get(get_claimed))
        .route("/api/item/:id", get(get_item))
        .route("/api/item/:id/claim", post(claim_item))
        .route("/api/item/:id/complete", post(complete_item))
        .route("/api/item/:id/fail", post(fail_item))
        .route("/api/item/:id/keepalive", post(keepalive_item))
        .route("/api/item/:id/stale-reset", post(stale_reset_item))
}

// Stale thresholds in ms (matches Node.js STALE_THRESHOLDS)
fn stale_threshold(preferred_executor: Option<&str>) -> u64 {
    match preferred_executor {
        Some("claude_cli") => 45 * 60 * 1000,
        Some("gpu") => 120 * 60 * 1000,
        Some("llm_server") => 60 * 60 * 1000,
        _ => 30 * 60 * 1000, // default
    }
}

async fn get_queue(State(state): State<Arc<AppState>>) -> Json<Value> {
    let q = state.queue.read().await;
    Json(json!({
        "items": q.items,
        "completed": q.completed
    }))
}

async fn get_stale(State(state): State<Arc<AppState>>) -> Json<Value> {
    let q = state.queue.read().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let stale: Vec<Value> = q
        .items
        .iter()
        .filter(|item| {
            item.get("status").and_then(|s| s.as_str()) == Some("in-progress")
                && item.get("claimedAt").and_then(|s| s.as_str()).is_some()
        })
        .filter_map(|item| {
            let claimed_at = item.get("claimedAt")?.as_str()?;
            let claimed_ms = chrono_parse_ms(claimed_at)?;
            let executor = item.get("preferred_executor").and_then(|s| s.as_str());
            let threshold = stale_threshold(executor);
            let age = now.saturating_sub(claimed_ms);
            if age > threshold {
                let mut v = item.clone();
                let obj = v.as_object_mut()?;
                obj.insert("staleMs".into(), json!(age));
                obj.insert("thresholdMs".into(), json!(threshold));
                obj.insert("staleMin".into(), json!(age / 60000));
                Some(v)
            } else {
                None
            }
        })
        .collect();

    Json(json!({"stale": stale, "count": stale.len()}))
}

async fn get_claimed(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    let q = state.queue.read().await;
    let claimed: Vec<&Value> = q
        .items
        .iter()
        .filter(|i| {
            i.get("status").and_then(|s| s.as_str()) == Some("in-progress")
                && i.get("claimedBy").and_then(|s| s.as_str()).is_some()
        })
        .collect();
    Json(json!({"ok": true, "claimed": claimed, "count": claimed.len()})).into_response()
}

async fn get_item(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> impl IntoResponse {
    let q = state.queue.read().await;
    let item = q
        .items
        .iter()
        .chain(q.completed.iter())
        .find(|i| i.get("id").and_then(|v| v.as_str()) == Some(&id));
    match item {
        Some(i) => (StatusCode::OK, Json(i.clone())).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Item not found"})),
        )
            .into_response(),
    }
}

async fn post_queue(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let title = match body.get("title").and_then(|t| t.as_str()) {
        Some(t) => t.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "title required"})),
            )
                .into_response()
        }
    };

    let priority_raw = body
        .get("priority")
        .and_then(|p| p.as_str())
        .unwrap_or("normal");
    let valid_priorities = ["critical", "high", "medium", "normal", "low", "idea"];
    let priority = if valid_priorities.contains(&priority_raw) {
        priority_raw
    } else {
        "normal"
    };
    let is_idea = priority == "idea";
    let skip_dedup = body
        .get("_skip_dedup")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !is_idea && !skip_dedup {
        let desc = body
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if desc.len() < 20 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "description_required",
                    "message": "description must be at least 20 characters"
                })),
            )
                .into_response();
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let item_id = body
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("wq-API-{}", chrono::Utc::now().timestamp_millis()));

    // Check for duplicate title in active items
    let mut q = state.queue.write().await;
    if !is_idea && !skip_dedup {
        let norm = |s: &str| {
            s.trim()
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let incoming_norm = norm(&title);
        let active_statuses = [
            "pending",
            "in-progress",
            "in_progress",
            "claimed",
            "incubating",
        ];
        let dup = q.items.iter().find(|i| {
            let status = i.get("status").and_then(|s| s.as_str()).unwrap_or("");
            active_statuses.contains(&status)
                && i.get("title")
                    .and_then(|t| t.as_str())
                    .map(|t| norm(t) == incoming_norm)
                    .unwrap_or(false)
        });
        if let Some(d) = dup {
            let dup_id = d
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let dup_title = d
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "ok": false,
                    "error": "duplicate",
                    "reason": "exact_title_dedup",
                    "duplicate_id": dup_id,
                    "duplicate_title": dup_title
                })),
            )
                .into_response();
        }
    }

    // scout_key dedup
    if let Some(scout_key) = body.get("scout_key").and_then(|s| s.as_str()) {
        let exists = q.items.iter().chain(q.completed.iter()).any(|i| {
            i.get("scout_key").and_then(|s| s.as_str()) == Some(scout_key)
                || i.get("tags")
                    .and_then(|t| t.as_array())
                    .map(|arr| arr.iter().any(|tag| tag.as_str() == Some(scout_key)))
                    .unwrap_or(false)
        });
        if exists {
            return (
                StatusCode::OK,
                Json(json!({"ok": false, "duplicate": true, "scout_key": scout_key})),
            )
                .into_response();
        }
    }

    // Infer preferred_executor
    let preferred_executor = if let Some(pe) =
        body.get("preferred_executor").and_then(|s| s.as_str())
    {
        pe.to_string()
    } else {
        let tags: Vec<&str> = body
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|arr| arr.iter().filter_map(|t| t.as_str()).collect())
            .unwrap_or_default();
        if tags.contains(&"gpu") || tags.contains(&"render") || tags.contains(&"simulation") {
            "gpu".to_string()
        } else if tags.contains(&"reasoning") || tags.contains(&"code") || tags.contains(&"complex")
        {
            "claude_cli".to_string()
        } else if tags.contains(&"heartbeat") || tags.contains(&"status") || tags.contains(&"poll")
        {
            "inference_key".to_string()
        } else if tags.contains(&"embedding")
            || tags.contains(&"local-llm")
            || tags.contains(&"peer-llm")
        {
            "llm_server".to_string()
        } else {
            let assignee = body
                .get("assignee")
                .and_then(|a| a.as_str())
                .unwrap_or("all");
            if assignee != "all" {
                "claude_cli".to_string()
            } else {
                "inference_key".to_string()
            }
        }
    };

    // Check ID collision
    let all_ids: std::collections::HashSet<&str> = q
        .items
        .iter()
        .chain(q.completed.iter())
        .filter_map(|i| i.get("id").and_then(|v| v.as_str()))
        .collect();
    let final_id = if all_ids.contains(item_id.as_str()) {
        format!("wq-API-{}", chrono::Utc::now().timestamp_millis())
    } else {
        item_id
    };

    let item = json!({
        "id": final_id,
        "itemVersion": 1,
        "created": now,
        "source": body.get("source").and_then(|s| s.as_str()).unwrap_or("api"),
        "assignee": body.get("assignee").and_then(|s| s.as_str()).unwrap_or("all"),
        "priority": priority,
        "status": "pending",
        "title": title,
        "description": body.get("description").and_then(|s| s.as_str()).unwrap_or(""),
        "notes": body.get("notes").and_then(|s| s.as_str()).unwrap_or(""),
        "preferred_executor": preferred_executor,
        "journal": [],
        "choices": body.get("choices").cloned().unwrap_or(json!([])),
        "choiceRecorded": null,
        "votes": [],
        "attempts": 0,
        "maxAttempts": body.get("maxAttempts").and_then(|n| n.as_u64()).unwrap_or(3),
        "claimedBy": null,
        "claimedAt": null,
        "completedAt": null,
        "result": null,
        "tags": body.get("tags").cloned().unwrap_or(json!([])),
        "scout_key": body.get("scout_key").cloned().unwrap_or(json!(null)),
        "repo": body.get("repo").cloned().unwrap_or(json!(null)),
        "project": body.get("project").or_else(|| body.get("repo")).cloned().unwrap_or(json!(null)),
    });

    q.items.push(item.clone());
    drop(q);

    flush_queue(&state).await;

    (StatusCode::CREATED, Json(json!({"ok": true, "item": item}))).into_response()
}

async fn claim_item(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let agent = match body
        .get("agent")
        .or_else(|| body.get("_author"))
        .and_then(|a| a.as_str())
    {
        Some(a) => a.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "agent required"})),
            )
                .into_response()
        }
    };

    let mut q = state.queue.write().await;
    let now = chrono::Utc::now().to_rfc3339();

    let item_pos = q
        .items
        .iter()
        .position(|i| i.get("id").and_then(|v| v.as_str()) == Some(&id));
    match item_pos {
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Item not found"})),
            )
                .into_response()
        }
        Some(pos) => {
            let item = &mut q.items[pos];
            let status = item
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let claimed_by = item
                .get("claimedBy")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();

            // Check if already claimed by someone else and not stale
            if !claimed_by.is_empty() && claimed_by != agent && status == "in-progress" {
                let claimed_at_str = item.get("claimedAt").and_then(|s| s.as_str()).unwrap_or("");
                if let Some(claimed_ms) = chrono_parse_ms(claimed_at_str) {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let executor = item.get("preferred_executor").and_then(|s| s.as_str());
                    let threshold = stale_threshold(executor);
                    if now_ms.saturating_sub(claimed_ms) < threshold {
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({
                                "error": format!("Already claimed by {}", claimed_by),
                                "claimedBy": claimed_by,
                                "claimedAt": claimed_at_str
                            })),
                        )
                            .into_response();
                    }
                }
            }

            if status != "pending" && status != "in-progress" {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": format!("Item is {}, cannot claim", status)
                    })),
                )
                    .into_response();
            }

            let obj = item.as_object_mut().unwrap();
            let prev_agent = obj
                .get("claimedBy")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            obj.insert("claimedBy".into(), json!(agent));
            obj.insert("claimedAt".into(), json!(now));
            obj.insert("keepaliveAt".into(), json!(now));
            obj.insert("status".into(), json!("in-progress"));
            let attempts = obj.get("attempts").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
            obj.insert("attempts".into(), json!(attempts));
            let version = obj.get("itemVersion").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
            obj.insert("itemVersion".into(), json!(version));

            let journal_entry = json!({
                "ts": now,
                "author": agent,
                "type": "claim",
                "text": match prev_agent {
                    Some(ref pa) => format!("Claimed (previous: {})", pa),
                    None => "Claimed".to_string()
                }
            });
            if let Some(j) = obj.get_mut("journal").and_then(|j| j.as_array_mut()) {
                j.push(journal_entry);
            } else {
                obj.insert("journal".into(), json!([journal_entry]));
            }

            let events_entry =
                json!({"ts": now, "agent": agent, "type": "claim", "note": body.get("note")});
            if let Some(e) = obj.get_mut("events").and_then(|e| e.as_array_mut()) {
                e.push(events_entry);
            } else {
                obj.insert("events".into(), json!([events_entry]));
            }

            let updated = item.clone();
            drop(q);
            flush_queue(&state).await;
            (StatusCode::OK, Json(json!({"ok": true, "item": updated}))).into_response()
        }
    }
}

async fn complete_item(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let agent = body
        .get("agent")
        .or_else(|| body.get("_author"))
        .and_then(|a| a.as_str())
        .unwrap_or("api")
        .to_string();

    let mut q = state.queue.write().await;
    let now = chrono::Utc::now().to_rfc3339();

    let item_pos = q
        .items
        .iter()
        .position(|i| i.get("id").and_then(|v| v.as_str()) == Some(&id));
    match item_pos {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Item not found"})),
        )
            .into_response(),
        Some(pos) => {
            let mut item = q.items.remove(pos);
            let obj = item.as_object_mut().unwrap();
            obj.insert("status".into(), json!("completed"));
            obj.insert("completedAt".into(), json!(now));
            if let Some(r) = body.get("resolution") {
                obj.insert("resolution".into(), r.clone());
            }
            if let Some(r) = body.get("result") {
                obj.insert("result".into(), r.clone());
            }
            let version = obj.get("itemVersion").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
            obj.insert("itemVersion".into(), json!(version));

            let text = body
                .get("resolution")
                .or_else(|| body.get("result"))
                .and_then(|v| v.as_str())
                .unwrap_or("Completed")
                .to_string();
            let journal_entry =
                json!({"ts": now, "author": agent, "type": "complete", "text": text});
            if let Some(j) = obj.get_mut("journal").and_then(|j| j.as_array_mut()) {
                j.push(journal_entry);
            } else {
                obj.insert("journal".into(), json!([journal_entry]));
            }
            let events_entry = json!({"ts": now, "agent": agent, "type": "complete", "note": text});
            if let Some(e) = obj.get_mut("events").and_then(|e| e.as_array_mut()) {
                e.push(events_entry);
            } else {
                obj.insert("events".into(), json!([events_entry]));
            }

            q.completed.push(item.clone());
            drop(q);
            flush_queue(&state).await;
            (StatusCode::OK, Json(json!({"ok": true, "item": item}))).into_response()
        }
    }
}

async fn fail_item(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let agent = body
        .get("agent")
        .or_else(|| body.get("_author"))
        .and_then(|a| a.as_str())
        .unwrap_or("api")
        .to_string();
    let reason = body
        .get("reason")
        .and_then(|r| r.as_str())
        .unwrap_or("Agent reported failure")
        .to_string();

    let mut q = state.queue.write().await;
    let now = chrono::Utc::now().to_rfc3339();

    let item_pos = q
        .items
        .iter()
        .position(|i| i.get("id").and_then(|v| v.as_str()) == Some(&id));
    match item_pos {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Item not found"})),
        )
            .into_response(),
        Some(pos) => {
            let item = &mut q.items[pos];
            let obj = item.as_object_mut().unwrap();
            let attempts = obj.get("attempts").and_then(|n| n.as_u64()).unwrap_or(0);
            let max_attempts = obj.get("maxAttempts").and_then(|n| n.as_u64()).unwrap_or(3);

            obj.insert("claimedBy".into(), json!(null));
            obj.insert("claimedAt".into(), json!(null));
            obj.insert("keepaliveAt".into(), json!(null));
            let version = obj.get("itemVersion").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
            obj.insert("itemVersion".into(), json!(version));

            let journal_entry = json!({"ts": now, "author": agent, "type": "fail", "text": reason});
            let events_entry = json!({"ts": now, "agent": agent, "type": "fail", "note": reason});

            if attempts >= max_attempts {
                obj.insert("status".into(), json!("blocked"));
                obj.insert(
                    "blockedReason".into(),
                    json!(format!(
                        "Exceeded maxAttempts ({}). Last failure: {}",
                        max_attempts, reason
                    )),
                );
                let dlq_entry = json!({"ts": now, "author": "rcc-api", "type": "dlq", "text": "Moved to blocked — maxAttempts exceeded"});
                if let Some(j) = obj.get_mut("journal").and_then(|j| j.as_array_mut()) {
                    j.push(journal_entry);
                    j.push(dlq_entry);
                } else {
                    obj.insert("journal".into(), json!([journal_entry, dlq_entry]));
                }
            } else {
                obj.insert("status".into(), json!("pending"));
                if let Some(j) = obj.get_mut("journal").and_then(|j| j.as_array_mut()) {
                    j.push(journal_entry);
                } else {
                    obj.insert("journal".into(), json!([journal_entry]));
                }
            }

            if let Some(e) = obj.get_mut("events").and_then(|e| e.as_array_mut()) {
                e.push(events_entry);
            } else {
                obj.insert("events".into(), json!([events_entry]));
            }

            let updated = item.clone();
            drop(q);
            flush_queue(&state).await;
            (StatusCode::OK, Json(json!({"ok": true, "item": updated}))).into_response()
        }
    }
}

async fn keepalive_item(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut q = state.queue.write().await;
    let now = chrono::Utc::now().to_rfc3339();

    let item_pos = q
        .items
        .iter()
        .position(|i| i.get("id").and_then(|v| v.as_str()) == Some(&id));
    match item_pos {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Item not found"})),
        )
            .into_response(),
        Some(pos) => {
            let item = &mut q.items[pos];
            let obj = item.as_object_mut().unwrap();
            obj.insert("keepaliveAt".into(), json!(now));
            if let Some(note) = body.get("note").and_then(|n| n.as_str()) {
                let entry = json!({"ts": now, "author": body.get("agent").and_then(|a| a.as_str()).unwrap_or("api"), "type": "keepalive", "text": note});
                if let Some(j) = obj.get_mut("journal").and_then(|j| j.as_array_mut()) {
                    j.push(entry);
                }
            }
            let updated = item.clone();
            drop(q);
            (StatusCode::OK, Json(json!({"ok": true, "item": updated}))).into_response()
        }
    }
}

async fn stale_reset_item(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !state.is_authed(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response();
    }
    let mut q = state.queue.write().await;
    let now = chrono::Utc::now().to_rfc3339();

    let item_pos = q
        .items
        .iter()
        .position(|i| i.get("id").and_then(|v| v.as_str()) == Some(&id));
    match item_pos {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Item not found"})),
        )
            .into_response(),
        Some(pos) => {
            let item = &mut q.items[pos];
            let obj = item.as_object_mut().unwrap();
            let prev_agent = obj
                .get("claimedBy")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            obj.insert("status".into(), json!("pending"));
            obj.insert("claimedBy".into(), json!(null));
            obj.insert("claimedAt".into(), json!(null));
            let attempts = obj.get("attempts").and_then(|n| n.as_u64()).unwrap_or(0) + 1;
            obj.insert("attempts".into(), json!(attempts));
            let entry = json!({"ts": now, "author": "rcc-api", "type": "stale-reset", "text": format!("Manual stale reset (was {})", prev_agent)});
            if let Some(j) = obj.get_mut("journal").and_then(|j| j.as_array_mut()) {
                j.push(entry);
            } else {
                obj.insert("journal".into(), json!([entry]));
            }
            let updated = item.clone();
            drop(q);
            flush_queue(&state).await;
            (StatusCode::OK, Json(json!({"ok": true, "item": updated}))).into_response()
        }
    }
}

fn chrono_parse_ms(s: &str) -> Option<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    Some(dt.timestamp_millis() as u64)
}
