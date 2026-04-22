//! AgentBus SSE listener daemon.
//!
//! Replaces bus-listener.sh. Connects to /bus/stream, dispatches:
//!   acc.update  → runs agent-pull.sh; touches work-signal
//!   acc.quench  → writes quench timestamp file
//!   acc.exec    → dispatches via exec_registry (or deprecated shell mode)
//!   work signals → touches work-signal

use std::time::Duration;

use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::time::sleep;

use crate::config::Config;
use crate::exec_registry;

#[derive(Debug, Deserialize)]
struct BusMessage {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    from: Option<String>,
    to: Option<String>,
    body: Option<Value>,
    #[allow(dead_code)]
    subject: Option<String>,
    /// MIME type for messages carrying inline content.
    mime: Option<String>,
    /// Encoding of `payload` field; "base64" for binary types.
    enc: Option<String>,
    /// Inline payload (base64-encoded when enc="base64").
    payload: Option<String>,
    /// Blob ID for messages referencing a stored blob.
    blob_id: Option<String>,
}

pub async fn run(args: &[String]) {
    let test_only = args.iter().any(|a| a == "--test");

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[bus] config error: {e}");
            std::process::exit(1);
        }
    };

    if test_only {
        println!("[bus] testing connection to {}/bus/stream ...", cfg.acc_url);
        let client = build_client();
        match client
            .get(format!("{}/bus/stream", cfg.acc_url))
            .header("Authorization", format!("Bearer {}", cfg.acc_token))
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) => {
                println!("[bus] connected, status {}", r.status());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[bus] connection failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let _ = std::fs::create_dir_all(cfg.acc_dir.join("logs"));

    log(&cfg, &format!(
        "Starting (agent={}, hub={})",
        cfg.agent_name, cfg.acc_url
    ));

    let client = build_client();
    let mut retry_delay = Duration::from_secs(5);

    loop {
        listen_once(&cfg, &client).await;
        log(&cfg, &format!("SSE disconnected — reconnecting in {retry_delay:?}"));
        sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(Duration::from_secs(120));
    }
}

async fn listen_once(cfg: &Config, client: &Client) {
    let url = format!("{}/bus/stream", cfg.acc_url);
    let resp = match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .header("Accept", "text/event-stream")
        .timeout(Duration::from_secs(3600))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            log(cfg, &format!("connect error: {e}"));
            return;
        }
    };

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                log(cfg, &format!("stream read error: {e}"));
                break;
            }
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find("\n\n") {
            let event = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();
            dispatch(cfg, client, &event).await;
        }
    }
}

async fn dispatch(cfg: &Config, client: &Client, event: &str) {
    let data = event
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|s| s.trim())
        .collect::<Vec<_>>()
        .join("");

    if data.is_empty() {
        return;
    }

    let msg: BusMessage = match serde_json::from_str(&data) {
        Ok(m) => m,
        Err(_) => return,
    };

    let msg_type = msg.msg_type.as_deref().unwrap_or("");
    let msg_to = msg.to.as_deref().unwrap_or("");

    if msg_to != "all" && msg_to != cfg.agent_name.as_str() && !msg_to.is_empty() {
        return;
    }

    match msg_type {
        "acc.update" => handle_update(cfg, &msg).await,
        "acc.quench" => handle_quench(cfg, &msg),
        "acc.exec" => handle_exec(cfg, client, &msg).await,
        "user.request" => handle_user_request(cfg, client, &msg).await,
        "soul.export" => handle_soul_export(cfg, client).await,
        "soul.import" => handle_soul_import(cfg, &msg).await,
        "soul.decommission" => handle_soul_decommission(cfg, &msg),
        "ping" => {
            let from = msg.from.as_deref().unwrap_or("?");
            log(cfg, &format!("ping from {from}"));
        }
        "project.arrived" | "queue.item.created" | "work.available" => {
            log(cfg, &format!("work signal: {msg_type}"));
            touch_work_signal(cfg);
        }
        "bus.blob_ready" => handle_blob_ready(cfg, &msg),
        "heartbeat" | "queue_sync" | "pong" | "handoff" | "blob" | "status-response" => {}
        other if !other.is_empty() => {
            log(cfg, &format!("unhandled type: {other} (to={msg_to})"));
        }
        _ => {}
    }
}

async fn handle_user_request(cfg: &Config, client: &Client, msg: &BusMessage) {
    let body = msg.body.as_ref().cloned().unwrap_or_default();
    let request_id = body.get("request_id")
        .or_else(|| body.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    if request_id.is_empty() {
        log(cfg, "user.request: missing request_id — skipping");
        return;
    }

    if try_claim_request(cfg, client, &request_id).await {
        log(cfg, &format!("user.request {request_id}: claimed — handling"));
        touch_work_signal(cfg);
    } else {
        log(cfg, &format!("user.request {request_id}: already claimed by peer — backing off"));
    }
}

async fn try_claim_request(cfg: &Config, client: &Client, request_id: &str) -> bool {
    let body = serde_json::json!({"agent": cfg.agent_name});
    let resp = client
        .post(format!("{}/api/requests/{}/claim", cfg.acc_url, request_id))
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    resp.map(|r| r.status().as_u16() == 200).unwrap_or(false)
}

async fn handle_update(cfg: &Config, msg: &BusMessage) {
    let component = str_field(&msg.body, "component");
    let branch = str_field(&msg.body, "branch");
    log(
        cfg,
        &format!(
            "acc.update — component={} branch={}",
            component.as_deref().unwrap_or("workspace"),
            branch.as_deref().unwrap_or("main")
        ),
    );

    let workspace = cfg.acc_dir.join("workspace");

    // ── Inline git pull (no bash dependency for the core pull logic) ──────────
    if workspace.join(".git").exists() {
        let ws = workspace.to_str().unwrap_or(".");

        // 1. git fetch origin --quiet
        let fetch = tokio::process::Command::new("git")
            .args(["-C", ws, "fetch", "origin", "--quiet"])
            .status()
            .await;
        match &fetch {
            Ok(s) if s.success() => {}
            Ok(s) => {
                log(cfg, &format!("WARNING: git fetch failed (exit {s}) — aborting update"));
                touch_work_signal(cfg);
                return;
            }
            Err(e) => {
                log(cfg, &format!("WARNING: git fetch error: {e} — aborting update"));
                touch_work_signal(cfg);
                return;
            }
        }

        // 2. Determine current branch
        let branch_out = tokio::process::Command::new("git")
            .args(["-C", ws, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .await;
        let current_branch = match branch_out {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => "main".to_string(),
        };

        // 3. git merge --ff-only origin/<branch> --quiet
        let merge = tokio::process::Command::new("git")
            .args(["-C", ws, "merge", "--ff-only", &format!("origin/{current_branch}"), "--quiet"])
            .status()
            .await;
        match &merge {
            Ok(s) if s.success() => log(cfg, &format!("git pull complete (branch={current_branch})")),
            Ok(s) => {
                log(cfg, &format!("WARNING: git merge --ff-only failed (exit {s}) — local changes? Skipping upgrade."));
                touch_work_signal(cfg);
                return;
            }
            Err(e) => {
                log(cfg, &format!("WARNING: git merge error: {e} — skipping upgrade"));
                touch_work_signal(cfg);
                return;
            }
        }
    } else {
        log(cfg, "WARNING: workspace .git not found — skipping pull");
    }

    // ── Run upgrade orchestrator (migrations → restarts → heartbeat) ──────────
    crate::upgrade::run_upgrade(cfg, crate::upgrade::UpgradeOptions { dry_run: false }).await;

    touch_work_signal(cfg);
}

fn handle_quench(cfg: &Config, msg: &BusMessage) {
    let minutes: u64 = msg
        .body
        .as_ref()
        .and_then(|b| b.get("minutes"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5);
    let reason = str_field(&msg.body, "reason").unwrap_or_else(|| "no reason".into());

    let until = chrono::Utc::now() + chrono::Duration::minutes(minutes as i64);
    let until_str = until.to_rfc3339();
    log(cfg, &format!("acc.quench: pausing {minutes}min until {until_str} — {reason}"));
    let _ = std::fs::write(cfg.quench_file(), &until_str);
}

async fn handle_exec(cfg: &Config, client: &Client, msg: &BusMessage) {
    // body may arrive as a JSON object or as a JSON-encoded string
    let body: Value = match msg.body.as_ref() {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };

    let exec_id = body.get("execId")
        .or_else(|| body.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    if exec_id.is_empty() {
        log(cfg, "acc.exec: missing execId — skipping");
        return;
    }

    // HMAC verification — enforced when agentbus_token is configured
    if !cfg.agentbus_token.is_empty() {
        let sig = body.get("sig").and_then(|v| v.as_str()).unwrap_or_default();
        if sig.is_empty() {
            log(cfg, &format!("acc.exec {exec_id}: missing HMAC sig — rejecting"));
            return;
        }
        let mut payload = body.clone();
        if let Some(obj) = payload.as_object_mut() {
            obj.remove("sig");
        }
        let expected = hmac_sign(&payload, &cfg.agentbus_token);
        if !bool::from(sig.as_bytes().ct_eq(expected.as_bytes())) {
            log(cfg, &format!("acc.exec {exec_id}: HMAC mismatch — rejecting"));
            return;
        }
    }

    let timeout_ms: u64 = body.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(30_000);
    let timeout_secs = (timeout_ms / 1000).max(1);

    // Target filter: body.targets must include our name or "all"
    let targeted = body.get("targets")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().any(|v| {
            v.as_str().map(|s| s == "all" || s == cfg.agent_name.as_str()).unwrap_or(false)
        }))
        .unwrap_or(true);

    if !targeted {
        return;
    }

    // Dispatch: command registry (structured) or deprecated shell mode
    if let Some(cmd_name) = body.get("command").and_then(|v| v.as_str()) {
        let cmd_name = cmd_name.to_string();
        let params = body.get("params").cloned().unwrap_or_default();

        let registry = exec_registry::CommandRegistry::load(&cfg.acc_dir);
        let cmd = match registry.find(&cmd_name) {
            Some(c) => c.clone(),
            None => {
                log(cfg, &format!("acc.exec {exec_id}: unknown command '{cmd_name}' — available: {:?}", registry.names()));
                post_exec_result(client, cfg, &exec_id, &format!("unknown command: {cmd_name}"), 1).await;
                return;
            }
        };

        log(cfg, &format!("acc.exec {exec_id}: command={cmd_name} timeout={timeout_ms}ms"));

        let acc_dir = cfg.acc_dir.clone();
        let agent_name = cfg.agent_name.clone();
        let acc_url = cfg.acc_url.clone();
        let acc_token = cfg.acc_token.clone();
        let client = client.clone();

        tokio::spawn(async move {
            let (output, exit_code) = exec_registry::execute(&cmd, &params, &acc_dir, timeout_secs).await;
            post_result(&client, &acc_url, &acc_token, &exec_id, &agent_name, &output, exit_code).await;
        });
    } else if let Some(code) = body.get("code").and_then(|v| v.as_str()) {
        log(cfg, &format!(
            "acc.exec {exec_id}: DEPRECATED shell mode — migrate caller to use command registry"
        ));
        let code = code.to_string();
        let agent_name = cfg.agent_name.clone();
        let acc_url = cfg.acc_url.clone();
        let acc_token = cfg.acc_token.clone();
        let client = client.clone();

        tokio::spawn(async move {
            let (output, exit_code) = run_shell(&code, timeout_secs).await;
            post_result(&client, &acc_url, &acc_token, &exec_id, &agent_name, &output, exit_code).await;
        });
    } else {
        log(cfg, &format!("acc.exec {exec_id}: neither 'command' nor 'code' field present — skipping"));
    }
}

async fn post_exec_result(client: &Client, cfg: &Config, exec_id: &str, output: &str, exit_code: i32) {
    post_result(client, &cfg.acc_url, &cfg.acc_token, exec_id, &cfg.agent_name, output, exit_code).await;
}

async fn post_result(
    client: &Client,
    acc_url: &str,
    acc_token: &str,
    exec_id: &str,
    agent_name: &str,
    output: &str,
    exit_code: i32,
) {
    let result = serde_json::json!({
        "agent": agent_name,
        "output": output,
        "exit_code": exit_code,
    });
    let url = format!("{acc_url}/api/exec/{exec_id}/result");
    let _ = client
        .post(&url)
        .header("Authorization", format!("Bearer {acc_token}"))
        .json(&result)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
}

async fn run_shell(code: &str, timeout_secs: u64) -> (String, i32) {
    use tokio::process::Command;
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("/bin/sh").arg("-c").arg(code).output(),
    )
    .await;

    match result {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.stderr.is_empty() {
                text.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            (text.trim_end().to_string(), out.status.code().unwrap_or(1))
        }
        Ok(Err(e)) => (format!("exec error: {e}"), 1),
        Err(_) => (format!("[timed out after {timeout_secs}s]"), 124),
    }
}

// ── Soul handlers ─────────────────────────────────────────────────────────────

/// Package ~/.hermes/ and ~/.acc/ into a tar.gz, hex-encode it, POST to server.
async fn handle_soul_export(cfg: &Config, client: &Client) {
    log(cfg, "soul.export: packaging agent soul");

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let tar_path = format!("/tmp/acc-soul-{}.tar.gz", cfg.agent_name);

    // Build tar excluding large runtime dirs
    let tar_out = tokio::process::Command::new("tar")
        .args([
            "-czf", &tar_path,
            "-C", &home,
            "--exclude=.acc/workspace",
            "--exclude=.acc/logs",
            "--exclude=.acc/bin",
            "--exclude=.acc/task-workspaces",
            "--exclude=.ccc/workspace",
            "--exclude=.ccc/logs",
            "--exclude=.ccc/bin",
        ])
        .arg(".hermes")
        .arg(if std::path::Path::new(&format!("{home}/.acc")).exists() { ".acc" } else { ".ccc" })
        .output()
        .await;

    match tar_out {
        Ok(o) if !o.status.success() => {
            log(cfg, &format!("soul.export: tar failed: {}", String::from_utf8_lossy(&o.stderr)));
            return;
        }
        Err(e) => {
            log(cfg, &format!("soul.export: tar error: {e}"));
            return;
        }
        _ => {}
    }

    let data = match tokio::fs::read(&tar_path).await {
        Ok(d) => d,
        Err(e) => { log(cfg, &format!("soul.export: read failed: {e}")); return; }
    };
    let _ = tokio::fs::remove_file(&tar_path).await;

    let tar_hex = hex::encode(&data);
    let exported_at = chrono::Utc::now().to_rfc3339();
    let payload = serde_json::json!({
        "agent": cfg.agent_name,
        "host": cfg.host,
        "tar_gz_hex": tar_hex,
        "exported_at": exported_at,
        "size_bytes": data.len(),
    });

    let url = format!("{}/api/agents/{}/soul/data", cfg.acc_url, cfg.agent_name);
    match client.post(&url)
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&payload)
        .timeout(Duration::from_secs(120))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            log(cfg, &format!("soul.export: uploaded {}B", data.len()));
        }
        Ok(r) => log(cfg, &format!("soul.export: server returned {}", r.status())),
        Err(e) => log(cfg, &format!("soul.export: upload failed: {e}")),
    }
}

/// Receive a new soul, overwrite local config, restart with new identity.
async fn handle_soul_import(cfg: &Config, msg: &BusMessage) {
    let body = msg.body.as_ref().cloned().unwrap_or_default();
    let new_name = match body.get("new_name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => { log(cfg, "soul.import: missing new_name"); return; }
    };
    let new_token = body.get("new_token").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let tar_hex = body.get("tar_gz_hex").and_then(|v| v.as_str()).unwrap_or("");

    log(cfg, &format!("soul.import: receiving identity '{new_name}'"));

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());

    // Unpack tarball if provided
    if !tar_hex.is_empty() {
        match hex::decode(tar_hex) {
            Ok(tar_bytes) => {
                let tar_path = format!("/tmp/acc-soul-import-{new_name}.tar.gz");
                if tokio::fs::write(&tar_path, &tar_bytes).await.is_ok() {
                    let status = tokio::process::Command::new("tar")
                        .args(["-xzf", &tar_path, "-C", &home,
                               "--exclude=.acc/.env",  // we rewrite this below
                               "--exclude=.ccc/.env"])
                        .status()
                        .await;
                    let _ = tokio::fs::remove_file(&tar_path).await;
                    match status {
                        Ok(s) if s.success() => log(cfg, "soul.import: files extracted"),
                        Ok(s) => log(cfg, &format!("soul.import: tar extract exited {s}")),
                        Err(e) => log(cfg, &format!("soul.import: tar extract error: {e}")),
                    }
                }
            }
            Err(e) => log(cfg, &format!("soul.import: hex decode failed: {e}")),
        }
    }

    // Rewrite ~/.acc/.env (or ~/.ccc/.env): update AGENT_NAME and token
    let acc_dir = if std::path::Path::new(&format!("{home}/.acc")).exists() {
        format!("{home}/.acc")
    } else {
        format!("{home}/.ccc")
    };
    let env_path = format!("{acc_dir}/.env");

    if let Ok(current_env) = std::fs::read_to_string(&env_path) {
        let updated: String = current_env.lines().map(|line| {
            if line.starts_with("AGENT_NAME=") {
                format!("AGENT_NAME={new_name}")
            } else if !new_token.is_empty() &&
                (line.starts_with("ACC_AGENT_TOKEN=") || line.starts_with("CCC_AGENT_TOKEN=")) {
                let key = line.split('=').next().unwrap_or("ACC_AGENT_TOKEN");
                format!("{key}={new_token}")
            } else {
                line.to_string()
            }
        }).collect::<Vec<_>>().join("\n") + "\n";
        if let Err(e) = std::fs::write(&env_path, &updated) {
            log(cfg, &format!("soul.import: failed to write .env: {e}"));
        } else {
            log(cfg, &format!("soul.import: updated .env (AGENT_NAME={new_name})"));
        }
    }

    // Rewrite ~/.acc/agent.json name field
    let agent_json_path = format!("{acc_dir}/agent.json");
    if let Ok(raw) = std::fs::read_to_string(&agent_json_path) {
        if let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&raw) {
            meta["name"] = serde_json::Value::String(new_name.clone());
            if let Ok(updated) = serde_json::to_string_pretty(&meta) {
                let _ = std::fs::write(&agent_json_path, updated);
            }
        }
    }

    log(cfg, &format!("soul.import: complete — restarting as '{new_name}'"));

    // Restart: exec the current binary with the same args so it re-reads .env
    let current_exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("acc-agent"));
    let args: Vec<String> = std::env::args().collect();
    let _ = std::process::Command::new(&current_exe)
        .args(&args[1..])
        .spawn();
    std::process::exit(0);
}

/// The source agent received confirmation it has been moved — exit cleanly.
fn handle_soul_decommission(cfg: &Config, msg: &BusMessage) {
    let reason = msg.body.as_ref()
        .and_then(|b| b.get("reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("no reason given");
    log(cfg, &format!("soul.decommission: exiting — {reason}"));
    std::process::exit(0);
}

fn handle_blob_ready(cfg: &Config, msg: &BusMessage) {
    let blob_id = msg.blob_id.as_deref().unwrap_or("?");
    let mime = msg.mime.as_deref().unwrap_or("unknown");
    log(cfg, &format!("bus.blob_ready: blob_id={blob_id} mime={mime}"));
}

/// Decode a message payload, handling base64 for binary types.
/// Returns raw bytes; callers convert to whatever representation they need.
#[allow(dead_code)]
fn decode_payload(msg: &BusMessage) -> Option<Vec<u8>> {
    let payload = msg.payload.as_deref()?;
    if msg.enc.as_deref() == Some("base64") {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.decode(payload).ok()
    } else {
        Some(payload.as_bytes().to_vec())
    }
}

fn hmac_sign(payload: &Value, secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key size");
    mac.update(payload.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn touch_work_signal(cfg: &Config) {
    let _ = std::fs::write(cfg.work_signal_file(), "");
}

fn str_field(body: &Option<Value>, key: &str) -> Option<String> {
    body.as_ref()?.get(key)?.as_str().map(String::from)
}

fn log(cfg: &Config, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{}] [bus] {msg}", cfg.agent_name);
    eprintln!("{line}");
    let log_path = cfg.log_file("bus-listener");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

fn build_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(3600))
        .build()
        .expect("failed to build HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_str_field_present() {
        let body = Some(serde_json::json!({"code": "echo hi", "mode": "shell"}));
        assert_eq!(str_field(&body, "code"), Some("echo hi".into()));
        assert_eq!(str_field(&body, "mode"), Some("shell".into()));
    }

    #[test]
    fn test_str_field_missing() {
        let body = Some(serde_json::json!({"a": 1}));
        assert_eq!(str_field(&body, "code"), None);
    }

    #[tokio::test]
    async fn test_run_shell_success() {
        let (out, code) = run_shell("echo hello", 5).await;
        assert_eq!(out, "hello");
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn test_run_shell_timeout() {
        let (out, code) = run_shell("sleep 10", 1).await;
        assert!(out.contains("timed out"));
        assert_eq!(code, 124);
    }

    #[tokio::test]
    async fn test_run_shell_exit_code() {
        let (_, code) = run_shell("exit 42", 5).await;
        assert_eq!(code, 42);
    }

    #[test]
    fn test_hmac_sign_deterministic() {
        let payload = serde_json::json!({"execId": "exec-123", "command": "ping"});
        let sig1 = hmac_sign(&payload, "secret");
        let sig2 = hmac_sign(&payload, "secret");
        assert_eq!(sig1, sig2);
        assert_ne!(sig1, hmac_sign(&payload, "other-secret"));
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    fn test_cfg_in_dir(dir: &std::path::Path, url: &str) -> Config {
        Config {
            acc_dir: dir.to_path_buf(),
            acc_url: url.to_string(),
            acc_token: "test-tok".to_string(),
            agent_name: "natasha".to_string(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: String::new(),
            ssh_user: "testuser".into(),
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
        }
    }

    fn mk_event(type_: &str, to: &str) -> String {
        format!(r#"data: {{"type":"{type_}","to":"{to}"}}"#)
    }

    fn mk_event_body(type_: &str, to: &str, body_json: &str) -> String {
        format!(r#"data: {{"type":"{type_}","to":"{to}","body":{body_json}}}"#)
    }

    // ── dispatch unit tests (no HTTP) ─────────────────────────────────────────

    #[tokio::test]
    async fn test_dispatch_ping_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://unused");
        let client = Client::new();
        dispatch(&cfg, &client, &mk_event("ping", "natasha")).await;
    }

    #[tokio::test]
    async fn test_dispatch_work_available_touches_signal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://unused");
        let client = Client::new();
        dispatch(&cfg, &client, &mk_event("work.available", "all")).await;
        assert!(cfg.work_signal_file().exists(), "work-signal must be created");
    }

    #[tokio::test]
    async fn test_dispatch_project_arrived_touches_signal() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://unused");
        let client = Client::new();
        dispatch(&cfg, &client, &mk_event("project.arrived", "natasha")).await;
        assert!(cfg.work_signal_file().exists());
    }

    #[tokio::test]
    async fn test_dispatch_quench_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://unused");
        let client = Client::new();
        dispatch(&cfg, &client, &mk_event_body("acc.quench", "all", r#"{"minutes":5}"#)).await;
        assert!(cfg.quench_file().exists(), "quench file must be written");
    }

    #[tokio::test]
    async fn test_dispatch_skips_wrong_target() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://unused");
        let client = Client::new();
        // to="boris" — natasha should ignore it
        dispatch(&cfg, &client, &mk_event("work.available", "boris")).await;
        assert!(!cfg.work_signal_file().exists(), "natasha must not react to boris's message");
    }

    #[tokio::test]
    async fn test_dispatch_malformed_json_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://unused");
        let client = Client::new();
        dispatch(&cfg, &client, "data: {not valid json}").await;
    }

    #[tokio::test]
    async fn test_dispatch_empty_event_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://unused");
        let client = Client::new();
        dispatch(&cfg, &client, "").await;
    }

    // ── listen_once SSE end-to-end tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_listen_once_hub_unreachable_returns_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_cfg_in_dir(dir.path(), "http://127.0.0.1:1");
        let client = Client::builder().timeout(Duration::from_secs(1)).build().unwrap();
        // Must not panic or hang.
        listen_once(&cfg, &client).await;
    }

    #[tokio::test]
    async fn test_listen_once_processes_ping_event() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::with_sse(vec![
            serde_json::json!({"type":"ping","from":"hub","to":"all"}).to_string(),
        ]).await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = build_client();
        listen_once(&cfg, &client).await;
        // ping leaves no observable side-effects — just verify no panic
    }

    #[tokio::test]
    async fn test_listen_once_quench_event_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::with_sse(vec![
            serde_json::json!({"type":"acc.quench","to":"all","body":{"minutes":10,"reason":"test"}}).to_string(),
        ]).await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = build_client();
        listen_once(&cfg, &client).await;
        assert!(cfg.quench_file().exists(), "quench file must exist after acc.quench event");
    }

    #[tokio::test]
    async fn test_listen_once_work_signal_event_touches_file() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::with_sse(vec![
            serde_json::json!({"type":"work.available","to":"all"}).to_string(),
        ]).await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = build_client();
        listen_once(&cfg, &client).await;
        assert!(cfg.work_signal_file().exists());
    }

    #[tokio::test]
    async fn test_listen_once_processes_multiple_events() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::with_sse(vec![
            serde_json::json!({"type":"acc.quench","to":"all","body":{"minutes":5}}).to_string(),
            serde_json::json!({"type":"work.available","to":"all"}).to_string(),
        ]).await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = build_client();
        listen_once(&cfg, &client).await;
        assert!(cfg.quench_file().exists(), "quench file from first event");
        assert!(cfg.work_signal_file().exists(), "work signal from second event");
    }

    #[tokio::test]
    async fn test_listen_once_skips_events_for_other_agents() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::with_sse(vec![
            serde_json::json!({"type":"work.available","to":"boris"}).to_string(),
        ]).await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = build_client();
        listen_once(&cfg, &client).await;
        assert!(!cfg.work_signal_file().exists(), "natasha must not react to boris's work signal");
    }

    #[tokio::test]
    async fn test_listen_once_handles_malformed_events_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::with_sse(vec![
            "not valid json at all".to_string(),
            serde_json::json!({"type":"work.available","to":"all"}).to_string(),
        ]).await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = build_client();
        listen_once(&cfg, &client).await;
        // valid event after bad one is still processed
        assert!(cfg.work_signal_file().exists());
    }

    // ── user.request first-responder tests ───────────────────────────────────

    #[tokio::test]
    async fn test_try_claim_request_success() {
        // Mock returns 200 → claim succeeds
        let mock = crate::hub_mock::HubMock::new().await;
        let client = Client::new();
        let cfg = test_cfg_in_dir(&tempfile::tempdir().unwrap().into_path(), &mock.url);
        let claimed = try_claim_request(&cfg, &client, "req-abc").await;
        assert!(claimed, "200 response should mean claim succeeded");
    }

    #[tokio::test]
    async fn test_try_claim_request_conflict() {
        use crate::hub_mock::HubState;
        let mock = crate::hub_mock::HubMock::with_state(
            HubState { request_claim_status: 409, ..Default::default() }
        ).await;
        let client = Client::new();
        let cfg = test_cfg_in_dir(&tempfile::tempdir().unwrap().into_path(), &mock.url);
        let claimed = try_claim_request(&cfg, &client, "req-abc").await;
        assert!(!claimed, "409 response should mean claim lost");
    }

    #[tokio::test]
    async fn test_dispatch_user_request_claims_and_touches_signal() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::new().await; // default 200
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = Client::new();
        dispatch(&cfg, &client, &format!(
            r#"data: {{"type":"user.request","to":"all","body":{{"request_id":"req-123"}}}}"#
        )).await;
        assert!(cfg.work_signal_file().exists(), "claim win must touch work-signal");
    }

    #[tokio::test]
    async fn test_dispatch_user_request_409_no_signal() {
        use crate::hub_mock::HubState;
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::with_state(
            HubState { request_claim_status: 409, ..Default::default() }
        ).await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = Client::new();
        dispatch(&cfg, &client, &format!(
            r#"data: {{"type":"user.request","to":"all","body":{{"request_id":"req-456"}}}}"#
        )).await;
        assert!(!cfg.work_signal_file().exists(), "claim loss must NOT touch work-signal");
    }

    #[tokio::test]
    async fn test_dispatch_user_request_no_id_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let mock = crate::hub_mock::HubMock::new().await;
        let cfg = test_cfg_in_dir(dir.path(), &mock.url);
        let client = Client::new();
        // body without request_id — should log and return silently
        dispatch(&cfg, &client, r#"data: {"type":"user.request","to":"all","body":{}}"#).await;
    }

    // ── post_result hub mock tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_post_result_success() {
        let mock = crate::hub_mock::HubMock::new().await;
        let client = Client::new();
        // post_result is fire-and-forget; just verify it doesn't panic.
        post_result(&client, &mock.url, "test-tok", "exec-abc", "natasha", "output text", 0).await;
    }

    #[tokio::test]
    async fn test_post_result_nonzero_exit() {
        let mock = crate::hub_mock::HubMock::new().await;
        let client = Client::new();
        post_result(&client, &mock.url, "test-tok", "exec-def", "boris", "stderr output", 1).await;
    }

    #[tokio::test]
    async fn test_post_result_hub_down_no_panic() {
        // post_result must not panic when the hub is unreachable.
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .build().unwrap();
        post_result(&client, "http://127.0.0.1:1", "tok", "exec-xyz", "agent", "out", 0).await;
    }
}
