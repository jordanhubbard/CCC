//! Fleet Monitor — orchestrates fleet-health, watchdog, and slack-ingest.
//! Port of scripts/cron-fleet-monitor.py
//!
//! Silence on success — only prints when something needs attention.

use serde_json::Value;
use std::process::{Command, Stdio};

fn main() {
    acc_tools::load_acc_env();

    // Find our own executable directory to co-locate the other tools
    let self_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let mut output_lines: Vec<String> = Vec::new();

    // fleet-health
    match run_tool(&self_dir.join("fleet-health")) {
        Ok((stdout, _stderr, 0)) if !stdout.is_empty() => {
            if health_has_alerts(&stdout) {
                output_lines.push(summarize_health(&stdout));
            }
        }
        Ok((_stdout, stderr, rc)) => {
            output_lines.push(format!(
                "Health check FAILED (rc={}): {}",
                rc,
                stderr.chars().take(200).collect::<String>()
            ));
        }
        Err(e) => output_lines.push(format!("Health check ERROR: {e}")),
    }

    // watchdog
    match run_tool(&self_dir.join("watchdog")) {
        Ok((stdout, _stderr, 0)) if !stdout.is_empty() => {
            if watchdog_has_alerts(&stdout) {
                output_lines.push(summarize_watchdog(&stdout));
            }
        }
        Ok((_stdout, stderr, rc)) => {
            output_lines.push(format!(
                "Watchdog FAILED (rc={}): {}",
                rc,
                stderr.chars().take(200).collect::<String>()
            ));
        }
        Err(e) => output_lines.push(format!("Watchdog ERROR: {e}")),
    }

    // slack-ingest (always silent on success)
    match run_tool(&self_dir.join("slack-ingest")) {
        Ok((_stdout, _stderr, 0)) => {} // silent
        Ok((_stdout, stderr, rc)) => {
            output_lines.push(format!(
                "Ingest FAILED (rc={}): {}",
                rc,
                stderr.chars().take(200).collect::<String>()
            ));
        }
        Err(e) => output_lines.push(format!("Ingest ERROR: {e}")),
    }

    if !output_lines.is_empty() {
        println!("{}", output_lines.join("\n"));
    }
}

fn run_tool(path: &std::path::Path) -> std::io::Result<(String, String, i32)> {
    let output = Command::new(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?
        .wait_with_output()?;
    let rc = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Ok((stdout, stderr, rc))
}

fn parse_json(s: &str) -> Option<Value> {
    serde_json::from_str(s).ok()
}

fn health_has_alerts(raw: &str) -> bool {
    let Some(data) = parse_json(raw) else {
        return true;
    };
    let down_svc = data["services"]
        .as_array()
        .map(|a| a.iter().any(|s| !s["ok"].as_bool().unwrap_or(true)))
        .unwrap_or(false);
    let down_prov = data["tokenhub_providers"]
        .as_array()
        .map(|a| a.iter().any(|p| !p["ok"].as_bool().unwrap_or(true)))
        .unwrap_or(false);
    let offline_agent = data["agents"]
        .as_array()
        .map(|a| a.iter().any(|ag| !ag["online"].as_bool().unwrap_or(true)))
        .unwrap_or(false);
    let remote_fail = data["remote_accfs"]
        .as_array()
        .map(|a| a.iter().any(|r| !r["ok"].as_bool().unwrap_or(true)))
        .unwrap_or(false);
    down_svc || down_prov || offline_agent || remote_fail
}

fn watchdog_has_alerts(raw: &str) -> bool {
    let Some(data) = parse_json(raw) else {
        return true;
    };
    data["alert_count"].as_u64().unwrap_or(0) > 0 || !data["healthy"].as_bool().unwrap_or(true)
}

fn summarize_health(raw: &str) -> String {
    let Some(data) = parse_json(raw) else {
        return format!(
            "Health check output parse error: {}",
            raw.chars().take(200).collect::<String>()
        );
    };
    let mut lines = Vec::new();
    let services = data["services"].as_array().cloned().unwrap_or_default();
    let down: Vec<&Value> = services
        .iter()
        .filter(|s| !s["ok"].as_bool().unwrap_or(true))
        .collect();
    let total = services.len();
    if !down.is_empty() {
        lines.push(format!("SERVICES: {}/{} up", total - down.len(), total));
        for s in &down {
            lines.push(format!(
                "  DOWN: {} — {}",
                s["name"].as_str().unwrap_or("?"),
                s["error"].as_str().unwrap_or("?")
            ));
        }
    } else {
        lines.push(format!("Services: all {} up", total));
    }
    let agents = data["agents"].as_array().cloned().unwrap_or_default();
    let offline: Vec<&Value> = agents
        .iter()
        .filter(|a| !a["online"].as_bool().unwrap_or(true))
        .collect();
    if !offline.is_empty() {
        let names: Vec<&str> = offline.iter().filter_map(|a| a["name"].as_str()).collect();
        lines.push(format!("AGENTS OFFLINE: {}", names.join(", ")));
    }
    lines.join("\n")
}

fn summarize_watchdog(raw: &str) -> String {
    let Some(data) = parse_json(raw) else {
        return format!(
            "Watchdog parse error: {}",
            raw.chars().take(200).collect::<String>()
        );
    };
    let alerts = data["alerts"].as_array().cloned().unwrap_or_default();
    let alert_count = alerts.len();
    let agents_online = data["agents_online"].as_u64().unwrap_or(0);
    let agents_total = data["agents_total"].as_u64().unwrap_or(0);
    if alert_count == 0 {
        return format!(
            "Watchdog: all clear ({}/{} agents online)",
            agents_online, agents_total
        );
    }
    let mut lines = Vec::new();
    if !data["healthy"].as_bool().unwrap_or(true) {
        lines.push(format!(
            "WATCHDOG ALERT — {} issue(s) detected",
            alert_count
        ));
    } else {
        lines.push(format!("Watchdog: {} notice(s)", alert_count));
    }
    for a in &alerts {
        match a["type"].as_str() {
            Some("stale_claim") => {
                let sev = a["severity"].as_str().unwrap_or("low");
                let emoji = match sev {
                    "high" | "critical" => "🟠",
                    "medium" => "🟡",
                    _ => "⚪",
                };
                let status = if a["agent_online"].as_bool().unwrap_or(true) {
                    "online"
                } else {
                    "OFFLINE"
                };
                lines.push(format!(
                    "  {} STALE: {} ({}) holding `{}` for {}min (threshold: {}min)",
                    emoji,
                    a["claimed_by"].as_str().unwrap_or("?"),
                    status,
                    a["task_id"].as_str().unwrap_or("?"),
                    a["claimed_minutes_ago"].as_i64().unwrap_or(0),
                    a["threshold_minutes"].as_i64().unwrap_or(0),
                ));
            }
            Some("offline_with_claims") => {
                let empty = vec![];
                let task_ids: Vec<&str> = a["tasks"]
                    .as_array()
                    .unwrap_or(&empty)
                    .iter()
                    .filter_map(|t| t["id"].as_str())
                    .collect();
                lines.push(format!(
                    "  🟠 OFFLINE: {} offline {}min with {} claimed task(s): {}",
                    a["agent"].as_str().unwrap_or("?"),
                    a["offline_minutes"].as_i64().unwrap_or(0),
                    a["claimed_task_count"].as_u64().unwrap_or(0),
                    task_ids.join(", "),
                ));
            }
            Some("unclaimed_old") => {
                lines.push(format!(
                    "  🟡 UNCLAIMED: `{}` ({}) pending {}h",
                    a["task_id"].as_str().unwrap_or("?"),
                    a["priority"].as_str().unwrap_or("?"),
                    a["age_hours"].as_f64().unwrap_or(0.0),
                ));
            }
            Some("blocked_task") => {
                lines.push(format!(
                    "  🟡 BLOCKED: `{}` — {}",
                    a["task_id"].as_str().unwrap_or("?"),
                    a["title"]
                        .as_str()
                        .unwrap_or("?")
                        .chars()
                        .take(50)
                        .collect::<String>(),
                ));
            }
            _ => {}
        }
    }
    lines.join("\n")
}
