//! ACC Stale Task Watchdog — detects stale/offline agent work items.
//! Port of scripts/stale-task-watchdog.py
//!
//! Outputs a JSON report to stdout. Runs read-only by default (WATCHDOG_DRY_RUN=true).

use chrono::{DateTime, Timelike, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;

fn main() {
    acc_tools::load_acc_env();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(run());
}

async fn run() {
    let base = acc_tools::acc_url();
    let token = acc_tools::acc_token();

    let stale_min: i64 = env_i64("STALE_CLAIM_MINUTES", 30);
    let offline_grace: i64 = env_i64("OFFLINE_GRACE_MINUTES", 10);
    let unclaimed_hours: f64 = std::env::var("UNCLAIMED_AGE_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24.0);
    let biz_start: u32 = env_i64("BUSINESS_HOURS_START", 8) as u32;
    let biz_end: u32 = env_i64("BUSINESS_HOURS_END", 22) as u32;
    let dry_run = std::env::var("WATCHDOG_DRY_RUN")
        .map(|v| v.to_lowercase() != "false")
        .unwrap_or(true);

    let client = reqwest::Client::new();
    let now = Utc::now();
    let mut report = json!({
        "timestamp": now.to_rfc3339(),
        "business_hours": is_business_hours(biz_start, biz_end),
        "dry_run": dry_run,
    });

    // Fetch agents
    let agents_data = get_json(&client, &base, "/api/agents", &token).await;
    let agents_list = agents_data["agents"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let agents_map: HashMap<String, Value> = agents_list
        .iter()
        .filter(|a| !a["decommissioned"].as_bool().unwrap_or(false))
        .filter_map(|a| a["name"].as_str().map(|n| (n.to_string(), a.clone())))
        .collect();

    report["agents_online"] = json!(agents_map
        .values()
        .filter(|a| a["online"].as_bool().unwrap_or(false))
        .count());
    report["agents_total"] = json!(agents_map.len());

    // Fetch stale tasks from server
    let stale_data = get_json(&client, &base, "/api/queue/stale", &token).await;
    let server_stale = stale_data["stale"].as_array().cloned().unwrap_or_default();
    report["server_stale_count"] = json!(server_stale.len());

    // Fetch claimed tasks
    let claimed_data = get_json(&client, &base, "/api/queue/claimed", &token).await;
    let claimed = claimed_data["claimed"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    report["claimed_count"] = json!(claimed.len());

    // Fetch full queue
    let queue_data = get_json(&client, &base, "/api/queue?exclude_completed=true", &token).await;
    let queue_items: Vec<Value> = queue_data["items"]
        .as_array()
        .cloned()
        .or_else(|| {
            if queue_data.is_array() {
                queue_data.as_array().cloned()
            } else {
                None
            }
        })
        .unwrap_or_else(|| [server_stale.clone(), claimed.clone()].concat());

    let mut all_alerts: Vec<Value> = Vec::new();

    // Check stale claims
    let stale_alerts = check_stale_claims(&queue_items, &agents_map, now, stale_min);
    all_alerts.extend(stale_alerts.clone());

    // Check offline agents with claims
    let offline_alerts = check_offline_agents(&queue_items, &agents_map, now, offline_grace);
    all_alerts.extend(offline_alerts);

    // Unclaimed old tasks (business hours only)
    if is_business_hours(biz_start, biz_end) {
        let unclaimed = check_unclaimed_old(&queue_items, now, unclaimed_hours);
        all_alerts.extend(unclaimed);
    }

    // Blocked tasks
    let blocked = check_blocked(&queue_items);
    all_alerts.extend(blocked.clone());

    // Auto-unclaim
    let mut released: Vec<String> = Vec::new();
    if !dry_run {
        for alert in &stale_alerts {
            if alert["severity"].as_str() == Some("high") {
                if let Some(id) = alert["task_id"].as_str() {
                    post_json(
                        &client,
                        &base,
                        &format!("/api/item/{id}/stale-reset"),
                        &token,
                        &json!({}),
                    )
                    .await;
                    released.push(id.to_string());
                }
            }
        }
    }
    report["auto_released"] = json!(released);

    // Sort by severity
    let sev_order = |s: &str| match s {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    };
    all_alerts.sort_by_key(|a| sev_order(a["severity"].as_str().unwrap_or("low")));

    let stale_count = stale_alerts.len();
    let offline_count = all_alerts
        .iter()
        .filter(|a| a["type"] == "offline_with_claims")
        .count();
    let unclaimed_count = all_alerts
        .iter()
        .filter(|a| a["type"] == "unclaimed_old")
        .count();
    let blocked_count = blocked.len();
    let high_sev = all_alerts
        .iter()
        .filter(|a| matches!(a["severity"].as_str(), Some("critical") | Some("high")))
        .count();

    report["alerts"] = json!(all_alerts);
    let alert_count = report["alerts"].as_array().map(|a| a.len()).unwrap_or(0);
    report["alert_count"] = json!(alert_count);
    report["alert_summary"] = json!({
        "stale_claims": stale_count,
        "offline_with_claims": offline_count,
        "unclaimed_old": unclaimed_count,
        "blocked": blocked_count,
    });
    report["healthy"] = json!(high_sev == 0);
    report["needs_attention"] = json!(high_sev > 0);

    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn is_business_hours(start: u32, end: u32) -> bool {
    // PDT = UTC-7
    let now = Utc::now();
    let hour_pdt = (now.hour() + 24 - 7) % 24;
    hour_pdt >= start && hour_pdt < end
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn stale_threshold(item: &Value, default_min: i64) -> Option<i64> {
    let priority = item["priority"].as_str().unwrap_or("normal");
    let executor = item["preferred_executor"].as_str().unwrap_or("");
    let prio_thresh: Option<i64> = match priority {
        "urgent" => Some(15),
        "high" => Some(30),
        "medium" => Some(45),
        "normal" => Some(30),
        "low" => Some(120),
        "idea" => return None,
        _ => Some(default_min),
    };
    let exec_thresh: i64 = match executor {
        "claude_cli" => 45,
        "gpu" => 120,
        "llm_server" => 60,
        _ => default_min,
    };
    Some(prio_thresh.unwrap_or(default_min).max(exec_thresh))
}

fn check_stale_claims(
    items: &[Value],
    agents: &HashMap<String, Value>,
    now: DateTime<Utc>,
    default_min: i64,
) -> Vec<Value> {
    let mut alerts = Vec::new();
    for item in items {
        if item["status"].as_str() != Some("in-progress") {
            continue;
        }
        let claimed_by = match item["claimedBy"].as_str() {
            Some(s) => s,
            None => continue,
        };
        let claimed_at = match item["claimedAt"].as_str().and_then(parse_ts) {
            Some(t) => t,
            None => continue,
        };
        let threshold = match stale_threshold(item, default_min) {
            Some(t) => t,
            None => continue,
        };
        let keepalive = item["keepaliveAt"].as_str().and_then(parse_ts);
        let last = keepalive.unwrap_or(claimed_at);
        let age_min = (now - last).num_seconds() / 60;
        if age_min > threshold {
            let agent_online = agents
                .get(claimed_by)
                .and_then(|a| a["online"].as_bool())
                .unwrap_or(false);
            alerts.push(json!({
                "type": "stale_claim",
                "severity": if !agent_online { "high" } else { "medium" },
                "task_id": item["id"],
                "title": item["title"].as_str().unwrap_or("").chars().take(80).collect::<String>(),
                "claimed_by": claimed_by,
                "agent_online": agent_online,
                "claimed_minutes_ago": age_min,
                "threshold_minutes": threshold,
                "priority": item["priority"],
                "last_activity": last.to_rfc3339(),
            }));
        }
    }
    alerts
}

fn check_offline_agents(
    items: &[Value],
    agents: &HashMap<String, Value>,
    now: DateTime<Utc>,
    grace_min: i64,
) -> Vec<Value> {
    let mut agent_claims: HashMap<&str, Vec<&Value>> = HashMap::new();
    for item in items {
        if item["status"].as_str() == Some("in-progress") {
            if let Some(cb) = item["claimedBy"].as_str() {
                agent_claims.entry(cb).or_default().push(item);
            }
        }
    }
    let mut alerts = Vec::new();
    for (name, claimed) in &agent_claims {
        let agent = match agents.get(*name) {
            Some(a) => a,
            None => continue,
        };
        if agent["online"].as_bool().unwrap_or(true) {
            continue;
        }
        let last_seen = agent["lastSeen"].as_str().and_then(parse_ts);
        let offline_min = last_seen
            .map(|ls| (now - ls).num_seconds() / 60)
            .unwrap_or(9999);
        if offline_min > grace_min {
            let tasks: Vec<Value> = claimed
                .iter()
                .map(|i| {
                    json!({
                        "id": i["id"],
                        "title": i["title"].as_str().unwrap_or("").chars().take(60).collect::<String>(),
                    })
                })
                .collect();
            alerts.push(json!({
                "type": "offline_with_claims",
                "severity": "high",
                "agent": name,
                "offline_minutes": offline_min,
                "claimed_task_count": claimed.len(),
                "tasks": tasks,
                "last_seen": last_seen.map(|ls| ls.to_rfc3339()),
            }));
        }
    }
    alerts
}

fn check_unclaimed_old(items: &[Value], now: DateTime<Utc>, default_hours: f64) -> Vec<Value> {
    let prio_hours = |p: &str| -> f64 {
        match p {
            "urgent" => 1.0,
            "high" => 6.0,
            "medium" | "normal" => 24.0,
            "low" => 72.0,
            _ => default_hours,
        }
    };
    let mut alerts = Vec::new();
    for item in items {
        if item["status"].as_str() != Some("pending") {
            continue;
        }
        let priority = item["priority"].as_str().unwrap_or("normal");
        if matches!(priority, "idea" | "incubating") {
            continue;
        }
        let created = match item["created"].as_str().and_then(parse_ts) {
            Some(t) => t,
            None => continue,
        };
        let age_h = (now - created).num_seconds() as f64 / 3600.0;
        let threshold = prio_hours(priority);
        if age_h > threshold {
            alerts.push(json!({
                "type": "unclaimed_old",
                "severity": if matches!(priority, "urgent" | "high") { "medium" } else { "low" },
                "task_id": item["id"],
                "title": item["title"].as_str().unwrap_or("").chars().take(80).collect::<String>(),
                "priority": priority,
                "assignee": item.get("assignee").and_then(|v| v.as_str()).unwrap_or("any"),
                "age_hours": (age_h * 10.0).round() / 10.0,
                "threshold_hours": threshold,
                "created": created.to_rfc3339(),
            }));
        }
    }
    alerts
}

fn check_blocked(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .filter(|i| i["status"].as_str() == Some("blocked"))
        .map(|i| {
            json!({
                "type": "blocked_task",
                "severity": "medium",
                "task_id": i["id"],
                "title": i["title"].as_str().unwrap_or("").chars().take(80).collect::<String>(),
                "priority": i["priority"],
                "blocked_reason": i["blockedReason"].as_str().unwrap_or("unknown").chars().take(200).collect::<String>(),
                "attempts": i["attempts"],
                "max_attempts": i["maxAttempts"],
            })
        })
        .collect()
}

async fn get_json(client: &reqwest::Client, base: &str, path: &str, token: &str) -> Value {
    let url = format!("{}{}", base, path);
    let req = client.get(&url);
    let req = if !token.is_empty() {
        req.bearer_auth(token)
    } else {
        req
    };
    match req.send().await {
        Ok(r) => r.json::<Value>().await.unwrap_or(json!({})),
        Err(_) => json!({}),
    }
}

async fn post_json(client: &reqwest::Client, base: &str, path: &str, token: &str, body: &Value) {
    let url = format!("{}{}", base, path);
    let req = client.post(&url).json(body);
    let req = if !token.is_empty() {
        req.bearer_auth(token)
    } else {
        req
    };
    let _ = req.send().await;
}
