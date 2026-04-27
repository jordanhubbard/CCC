//! CCC Fleet Health Check — validates all core services.
//! Port of scripts/fleet-health-check.py

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::time::Instant;

#[tokio::main]
async fn main() {
    acc_tools::load_acc_env();

    let ccc_api = std::env::var("CCC_API").unwrap_or_else(|_| "http://localhost:8789".to_string());
    let qdrant_url = std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://localhost:6333".to_string());
    let minio_url = std::env::var("MINIO_ENDPOINT").unwrap_or_else(|_| "http://localhost:9000".to_string());
    let tokenhub_url = std::env::var("TOKENHUB_URL").unwrap_or_else(|_| "http://localhost:8090".to_string());
    let searxng_url = std::env::var("SEARXNG_URL").unwrap_or_else(|_| "http://localhost:8888".to_string());
    let qdrant_api_key = acc_tools::resolve_qdrant_api_key();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    let now = Utc::now();
    let mut report = json!({
        "timestamp": now.to_rfc3339(),
        "hub": "rocky@do-host1",
    });

    // 1. Core services
    let mut services: Vec<Value> = Vec::new();
    services.push(probe(&client, "ccc-api", &format!("{ccc_api}/api/health"), None, Some("ok")).await);
    services.push(probe(&client, "agentbus", &format!("{ccc_api}/api/health"), None, Some("ok")).await);
    services.push(probe(
        &client,
        "qdrant",
        &format!("{qdrant_url}/healthz"),
        qdrant_api_key.as_deref().map(|k| ("api-key", k)),
        None,
    ).await);
    services.push(probe(&client, "minio", &format!("{minio_url}/minio/health/live"), None, None).await);
    services.push(probe(&client, "tokenhub", &format!("{tokenhub_url}/v1/models"), None, None).await);
    services.push(probe(&client, "searxng", &format!("{searxng_url}/healthz"), None, None).await);

    // Redis
    services.push(probe_redis());

    // AccFS gateway
    services.push(probe(&client, "accfs-gateway", "http://127.0.0.1:9100/minio/health/live", None, None).await);

    // AccFS FUSE mount
    let fuse_mounted = std::path::Path::new("/mnt/accfs").exists();
    services.push(json!({
        "name": "accfs-fuse",
        "url": "/mnt/accfs",
        "ok": fuse_mounted,
        "status": if fuse_mounted { "mounted" } else { "not mounted" },
        "latency_ms": 0,
        "error": if fuse_mounted { Value::Null } else { json!("/mnt/accfs is not mounted") },
    }));

    // Docker containers
    services.extend(check_docker_containers());

    // Tailscale
    services.push(check_tailscale());

    report["services"] = json!(services);

    // 2. Sentinel
    let sentinel = check_sentinel();
    report["sentinel"] = sentinel;

    // 3. Agent health
    let token = acc_tools::acc_token();
    let agents = get_agents(&client, &ccc_api, &token).await;
    let agent_results: Vec<Value> = agents.iter().map(|a| check_agent_health(a, now)).collect();
    report["agents"] = json!(agent_results);

    // 4. Summary
    let svc_ok = services.iter().filter(|s| s["ok"].as_bool().unwrap_or(false)).count();
    let svc_total = services.len();
    let failed_svcs: Vec<&str> = services
        .iter()
        .filter(|s| !s["ok"].as_bool().unwrap_or(true))
        .filter_map(|s| s["name"].as_str())
        .collect();
    let stale_agents: Vec<&str> = agent_results
        .iter()
        .filter(|a| !a["ok"].as_bool().unwrap_or(true))
        .filter_map(|a| a["name"].as_str())
        .collect();
    let all_ok = failed_svcs.is_empty();
    report["summary"] = json!({
        "all_ok": all_ok,
        "services_ok": format!("{}/{}", svc_ok, svc_total),
        "agents_ok": format!("{}/{}", agent_results.iter().filter(|a| a["ok"].as_bool().unwrap_or(false)).count(), agent_results.len()),
        "failed_services": failed_svcs,
        "stale_agents": stale_agents,
    });

    // 5. Tokenhub providers
    let tokenhub_provs = check_tokenhub_providers(&client, &tokenhub_url).await;
    if !tokenhub_provs.is_empty() {
        report["tokenhub_providers"] = json!(tokenhub_provs);
    }

    // 6. Remote AccFS
    let remote = check_remote_accfs();
    if !remote.is_empty() {
        report["remote_accfs"] = json!(remote);
    }

    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

async fn probe(
    client: &reqwest::Client,
    name: &str,
    url: &str,
    header: Option<(&str, &str)>,
    expect: Option<&str>,
) -> Value {
    let t0 = Instant::now();
    let req = client.get(url);
    let req = if let Some((k, v)) = header {
        req.header(k, v)
    } else {
        req
    };
    match req.send().await {
        Ok(resp) => {
            let latency = t0.elapsed().as_millis() as i64;
            let status = resp.status().as_u16();
            let ok_status = status == 200 || status == 204;
            let body = resp.text().await.unwrap_or_default();
            let ok = ok_status && expect.map(|e| body.contains(e)).unwrap_or(true);
            json!({
                "name": name,
                "url": url,
                "ok": ok,
                "status": status,
                "latency_ms": latency,
                "error": if ok { Value::Null } else { json!(body.chars().take(200).collect::<String>()) },
            })
        }
        Err(e) => json!({
            "name": name,
            "url": url,
            "ok": false,
            "status": 0,
            "latency_ms": 0,
            "error": e.to_string(),
        }),
    }
}

fn probe_redis() -> Value {
    let r = std::process::Command::new("redis-cli")
        .args(["-h", "127.0.0.1", "-p", "6379", "ping"])
        .output();
    match r {
        Ok(out) => {
            let ok = out.status.success()
                && String::from_utf8_lossy(&out.stdout).contains("PONG");
            json!({
                "name": "redis",
                "url": "redis://127.0.0.1:6379",
                "ok": ok,
                "status": if ok { 200 } else { 0 },
                "latency_ms": 0,
                "error": if ok { Value::Null } else { json!(String::from_utf8_lossy(&out.stdout).trim().to_string()) },
            })
        }
        Err(e) => json!({
            "name": "redis",
            "url": "redis://127.0.0.1:6379",
            "ok": false,
            "status": 0,
            "latency_ms": 0,
            "error": e.to_string(),
        }),
    }
}

fn check_docker_containers() -> Vec<Value> {
    let expected = ["qdrant", "searxng"];
    let r = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}|{{.Status}}"])
        .output();
    match r {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let running: std::collections::HashMap<&str, &str> = text
                .lines()
                .filter_map(|l| l.split_once('|'))
                .collect();
            expected
                .iter()
                .map(|c| {
                    let status = running.get(c).copied().unwrap_or("not found");
                    let ok = running.get(c).map(|s| s.contains("Up")).unwrap_or(false);
                    json!({
                        "name": format!("docker:{c}"),
                        "url": "docker",
                        "ok": ok,
                        "status": status,
                        "latency_ms": 0,
                        "error": if ok { Value::Null } else { json!(format!("container {c} not running")) },
                    })
                })
                .collect()
        }
        Err(e) => vec![json!({
            "name": "docker",
            "url": "docker",
            "ok": false,
            "status": 0,
            "latency_ms": 0,
            "error": e.to_string(),
        })],
    }
}

fn check_tailscale() -> Value {
    let r = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output();
    match r {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Ok(ts) = serde_json::from_str::<Value>(&text) {
                let state = ts["BackendState"].as_str().unwrap_or("unknown");
                let ok = state == "Running";
                return json!({
                    "name": "tailscaled",
                    "url": "tailscale",
                    "ok": ok,
                    "status": state,
                    "latency_ms": 0,
                    "error": if ok { Value::Null } else { json!("tailscale not running") },
                });
            }
            json!({
                "name": "tailscaled",
                "url": "tailscale",
                "ok": false,
                "status": 0,
                "latency_ms": 0,
                "error": "could not parse tailscale status",
            })
        }
        Err(e) => json!({
            "name": "tailscaled",
            "url": "tailscale",
            "ok": false,
            "status": 0,
            "latency_ms": 0,
            "error": e.to_string(),
        }),
    }
}

fn check_sentinel() -> Value {
    let id = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{:x}", md5::compute(nanos.to_le_bytes()))
    };
    let sentinel = json!({"id": &id, "from": "rocky"});
    let write_ok = {
        let mut cmd = std::process::Command::new("mc");
        cmd.args(["pipe", "local/agents/shared/health-sentinel.json"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Ok(mut child) = cmd.spawn() {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(sentinel.to_string().as_bytes());
            }
            child.wait().map(|s| s.success()).unwrap_or(false)
        } else {
            false
        }
    };
    if !write_ok {
        return json!({"ok": false, "id": id, "verified": false});
    }
    // Read back
    let read_id = std::process::Command::new("mc")
        .args(["cat", "local/agents/shared/health-sentinel.json"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v["id"].as_str().map(|s| s.to_string()));
    let verified = read_id.as_deref() == Some(&id);
    json!({"ok": verified, "id": id, "verified": verified})
}

async fn get_agents(client: &reqwest::Client, base: &str, token: &str) -> Vec<Value> {
    let url = format!("{base}/api/agents");
    let req = client.get(&url);
    let req = if !token.is_empty() {
        req.bearer_auth(token)
    } else {
        req
    };
    match req.send().await {
        Ok(r) => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["agents"].as_array().cloned())
            .unwrap_or_default(),
        Err(_) => vec![],
    }
}

fn check_agent_health(agent: &Value, now: DateTime<Utc>) -> Value {
    let name = agent["name"].as_str().unwrap_or("unknown");
    let online = agent["online"].as_bool().unwrap_or(false);
    let last_seen = agent["lastSeen"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let stale_min = last_seen.map(|ls| (now - ls).num_seconds() / 60);
    let stale = stale_min.map(|m| m > 10).unwrap_or(false);
    let mut issues: Vec<&str> = Vec::new();
    if stale {
        issues.push("last heartbeat >10m ago");
    }
    if !online {
        issues.push("marked offline");
    }
    json!({
        "name": name,
        "host": agent.get("host").and_then(|h| h.as_str()).unwrap_or("unknown"),
        "online": online,
        "ok": !stale,
        "stale_minutes": stale_min,
        "issues": issues,
    })
}

fn check_remote_accfs() -> Vec<Value> {
    let nodes = [
        ("sparky", "100.87.229.125", "jkh"),
        ("puck", "100.87.68.11", "jkh"),
    ];
    nodes
        .iter()
        .map(|(node, ip, user)| {
            let cmd = "~/bin/mc ls accfs/accfs/ >/dev/null 2>&1 && echo ACCFS_OK || echo ACCFS_FAIL";
            let r = std::process::Command::new("ssh")
                .args([
                    "-o", "ConnectTimeout=5",
                    "-o", "StrictHostKeyChecking=no",
                    &format!("{user}@{ip}"),
                    cmd,
                ])
                .output();
            match r {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let ok = stdout.contains("ACCFS_OK");
                    let error = if ok {
                        Value::Null
                    } else {
                        json!(String::from_utf8_lossy(&out.stderr)
                            .trim()
                            .chars()
                            .take(200)
                            .collect::<String>())
                    };
                    json!({ "name": format!("accfs-access:{node}"), "ok": ok, "error": error })
                }
                Err(e) => json!({
                    "name": format!("accfs-access:{node}"),
                    "ok": false,
                    "error": e.to_string(),
                }),
            }
        })
        .collect()
}

async fn check_tokenhub_providers(client: &reqwest::Client, tokenhub_url: &str) -> Vec<Value> {
    let admin_token = std::env::var("TOKENHUB_ADMIN_TOKEN").unwrap_or_default();
    if admin_token.is_empty() {
        return vec![];
    }
    let url = format!("{tokenhub_url}/admin/v1/health");
    let resp = match client.get(&url).bearer_auth(&admin_token).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return vec![],
    };
    let data: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let provs = data["providers"]
        .as_array()
        .or_else(|| data["items"].as_array())
        .cloned()
        .unwrap_or_default();
    provs
        .iter()
        .map(|p| {
            let name = p["provider_id"]
                .as_str()
                .or_else(|| p["name"].as_str())
                .or_else(|| p["id"].as_str())
                .unwrap_or("unknown");
            let state = p["state"].as_str().unwrap_or("");
            let ok = state == "healthy";
            json!({
                "name": name,
                "ok": ok,
                "state": state,
                "error": if ok { Value::Null } else { p["last_error"].clone() },
            })
        })
        .collect()
}
