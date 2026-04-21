//! Hermes session driver.
//!
//! Replaces hermes-driver.py. Wraps the hermes CLI with:
//!   - auto-resume on budget exhaustion (up to 6 attempts)
//!   - CCC heartbeat/keepalive posting during execution
//!   - three modes: --item <id>, --resume <session-id>, --poll

use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

use crate::config::Config;

const MAX_RESUME_ATTEMPTS: u32 = 6;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const CLAUDE_ONLY_TAGS: &[&str] = &["claude", "claude_cli"];

pub async fn run(args: &[String]) {
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[hermes] config error: {e}");
            std::process::exit(1);
        }
    };

    let mut item_id: Option<String> = None;
    let mut query: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut poll = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--item" => { i += 1; item_id = args.get(i).cloned(); }
            "--query" => { i += 1; query = args.get(i).cloned(); }
            "--resume" => { i += 1; session_id = args.get(i).cloned(); }
            "--poll" => poll = true,
            _ => {}
        }
        i += 1;
    }

    let client = build_client();

    if poll {
        poll_queue(&cfg, &client).await;
    } else if session_id.is_some() || query.is_some() || item_id.is_some() {
        run_task(&cfg, &client, query, item_id, session_id).await;
    } else {
        eprintln!("[hermes] one of --item, --query, --resume, --poll required");
        std::process::exit(1);
    }
}

async fn run_task(
    cfg: &Config,
    client: &reqwest::Client,
    query: Option<String>,
    item_id: Option<String>,
    session_id: Option<String>,
) {
    log(cfg, &format!(
        "starting task item={:?} session={:?} query={}",
        item_id,
        session_id,
        query.as_deref().map(|q| &q[..q.len().min(60)]).unwrap_or("none")
    ));

    // Claim the item if we have an ID and no existing session (first run)
    if let (Some(id), None) = (&item_id, &session_id) {
        if !api_claim(cfg, client, id).await {
            log(cfg, &format!("claim rejected for {id} — aborting"));
            return;
        }
    }

    // Prepend workspace context to query on first attempt
    let effective_query = if session_id.is_none() {
        if let (Some(q), Some(ws)) = (&query, std::env::var("TASK_WORKSPACE_LOCAL").ok()) {
            Some(format!(
                "Your task workspace is: {ws}\n\
                 Work only within this directory. Do NOT run git commit or git push — \
                 the queue-worker handles that.\n\n{q}"
            ))
        } else {
            query.clone()
        }
    } else {
        query.clone()
    };

    let mut current_session = session_id;
    let mut final_output = String::new();
    let mut completed = false;
    let hermes_bin = find_hermes();

    for attempt in 1..=MAX_RESUME_ATTEMPTS {
        log(cfg, &format!("attempt {attempt}/{MAX_RESUME_ATTEMPTS} session={current_session:?}"));
        post_heartbeat(cfg, client, &format!("hermes attempt {attempt}")).await;

        // Keepalive task
        let ka_cfg = cfg.clone();
        let ka_client = client.clone();
        let ka_item = item_id.clone();
        let ka_sess = current_session.clone();
        let ka_att = attempt;
        let (ka_tx, mut ka_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(KEEPALIVE_INTERVAL);
            interval.tick().await; // skip first immediate tick
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let note = format!("hermes running (attempt {ka_att}, session={ka_sess:?})");
                        post_heartbeat(&ka_cfg, &ka_client, &note).await;
                        if let Some(ref id) = ka_item {
                            post_keepalive(&ka_cfg, &ka_client, id, &note).await;
                        }
                    }
                    _ = &mut ka_rx => break,
                }
            }
        });

        let (output, ok, new_sess) = invoke_hermes(
            cfg,
            &hermes_bin,
            if attempt == 1 { effective_query.as_deref() } else { None },
            current_session.as_deref(),
            item_id.as_deref(),
        )
        .await;

        let _ = ka_tx.send(());

        final_output = output;
        if let Some(ns) = new_sess {
            if Some(&ns) != current_session.as_ref() {
                log(cfg, &format!("session rotated → {ns}"));
                current_session = Some(ns);
            }
        }

        if ok {
            log(cfg, &format!("completed after {attempt} attempt(s)"));
            completed = true;
            break;
        }

        log(cfg, &format!("exited incomplete: {}", &final_output[..final_output.len().min(200)]));
        if attempt < MAX_RESUME_ATTEMPTS {
            sleep(Duration::from_secs(5)).await;
        }
    }

    if let Some(ref id) = item_id {
        if completed {
            post_complete(cfg, client, id, &final_output).await;
        } else {
            let reason = format!(
                "Hermes did not complete after {MAX_RESUME_ATTEMPTS} attempts. Last output: {}",
                &final_output[..final_output.len().min(500)]
            );
            post_fail(cfg, client, id, &reason).await;
        }
    }
}

async fn invoke_hermes(
    cfg: &Config,
    hermes_bin: &str,
    query: Option<&str>,
    session_id: Option<&str>,
    item_id: Option<&str>,
) -> (String, bool, Option<String>) {
    let uses_subcommands = detect_subcommands(hermes_bin).await;

    let cmd_args: Vec<String> = if uses_subcommands {
        let mut a = vec!["chat".into(), "--max-turns".into(), "120".into(), "-Q".into()];
        if let Some(sid) = session_id {
            a.push("--resume".into());
            a.push(sid.into());
        } else if let Some(q) = query {
            a.push("-q".into());
            a.push(q.into());
        }
        a
    } else {
        let mut a = vec!["--max-iterations".into(), "120".into(), "--quiet".into()];
        if let Some(sid) = session_id {
            a.push("--resume".into());
            a.push(sid.into());
        } else if let Some(q) = query {
            a.push("--query".into());
            a.push(q.into());
        }
        a
    };

    if query.is_none() && session_id.is_none() {
        return ("no query or session provided".into(), false, None);
    }

    let timeout_secs = 120 * 120u64; // 4h generous wall-clock limit
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new(hermes_bin)
            .args(&cmd_args)
            .env_opt("CCC_QUEUE_ITEM_ID", item_id)
            .env_opt("HERMES_PLATFORM", Some("cli"))
            .env_opt("HERMES_QUIET", Some("1"))
            .output(),
    )
    .await;

    match result {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let output = if stdout.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                stdout.trim().to_string()
            };
            log(cfg, &format!("hermes exited {} ({} chars)", out.status.code().unwrap_or(-1), output.len()));

            // Try to extract new session ID from output
            let new_session = extract_session_id(&stdout);
            let success = out.status.success();
            (output, success, new_session)
        }
        Ok(Err(e)) => (format!("exec error: {e}"), false, None),
        Err(_) => (format!("[timed out after {timeout_secs}s]"), false, None),
    }
}

async fn detect_subcommands(hermes_bin: &str) -> bool {
    let r = Command::new(hermes_bin)
        .args(["chat", "--help"])
        .output()
        .await;
    r.map(|o| o.status.success()).unwrap_or(false)
}

fn extract_session_id(output: &str) -> Option<String> {
    for line in output.lines() {
        let lower = line.to_lowercase();
        if lower.contains("session_id:") || lower.contains("session:") {
            // Session IDs look like: 20260417_153022_abc123
            for word in line.split_whitespace() {
                if word.starts_with("20") && word.contains('_') && word.len() > 15 {
                    return Some(word.to_string());
                }
            }
        }
    }
    None
}

fn find_hermes() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.local/bin/hermes"),
        "/usr/local/bin/hermes".into(),
        "/opt/homebrew/bin/hermes".into(),
    ];
    for candidate in &candidates {
        if std::path::Path::new(candidate).exists() {
            return candidate.clone();
        }
    }
    // Search PATH
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let p = std::path::PathBuf::from(dir).join("hermes");
            if p.exists() {
                return p.to_str().unwrap_or("hermes").to_string();
            }
        }
    }
    "hermes".into()
}

// Queue poll mode
async fn poll_queue(cfg: &Config, client: &reqwest::Client) {
    log(cfg, &format!("starting queue poll (agent={}, hub={})", cfg.agent_name, cfg.acc_url));
    loop {
        if let Some(item) = fetch_hermes_item(cfg, client).await {
            let id = item["id"].as_str().unwrap_or("").to_string();
            let title = item["title"].as_str().unwrap_or("").to_string();
            log(cfg, &format!("found hermes task: {id} — {title}"));
            let query = format!(
                "{}\n\n{}",
                item["title"].as_str().unwrap_or(""),
                item["description"].as_str().unwrap_or("")
            );
            run_task(cfg, client, Some(query), Some(id), None).await;
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn fetch_hermes_item(cfg: &Config, client: &reqwest::Client) -> Option<serde_json::Value> {
    let resp = client
        .get(format!("{}/api/queue", cfg.acc_url))
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .ok()?;
    let data: serde_json::Value = resp.json().await.ok()?;
    let items = data.as_array()
        .cloned()
        .or_else(|| data["items"].as_array().cloned())?;
    for item in items {
        if item["status"].as_str() != Some("pending") { continue; }
        let assignee = item["assignee"].as_str().unwrap_or("");
        if !assignee.is_empty() && assignee != "all" && assignee != cfg.agent_name.as_str() {
            continue;
        }
        // Skip tasks explicitly reserved for claude CLI
        let tags: Vec<&str> = item["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let preferred = item["preferred_executor"].as_str().unwrap_or("");
        let is_claude_only = preferred == "claude_cli"
            || tags.iter().any(|t| CLAUDE_ONLY_TAGS.contains(t));
        if !is_claude_only {
            return Some(item);
        }
    }
    None
}

// ── API helpers ────────────────────────────────────────────────────────────────

async fn api_claim(cfg: &Config, client: &reqwest::Client, item_id: &str) -> bool {
    let body = serde_json::json!({"agent": cfg.agent_name, "note": "hermes-driver claiming"});
    let resp = client
        .post(format!("{}/api/item/{item_id}/claim", cfg.acc_url))
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    resp.map(|r| r.status().is_success()).unwrap_or(false)
}

async fn post_heartbeat(cfg: &Config, client: &reqwest::Client, note: &str) {
    let body = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "status": "ok",
        "note": &note[..note.len().min(200)],
    });
    let _ = client
        .post(format!("{}/api/heartbeat/{}", cfg.acc_url, cfg.agent_name))
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
}

async fn post_keepalive(cfg: &Config, client: &reqwest::Client, item_id: &str, note: &str) {
    let body = serde_json::json!({"agent": cfg.agent_name, "note": note});
    let _ = client
        .post(format!("{}/api/item/{item_id}/keepalive", cfg.acc_url))
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
}

async fn post_complete(cfg: &Config, client: &reqwest::Client, item_id: &str, result: &str) {
    let truncated = &result[..result.len().min(4000)];
    let body = serde_json::json!({
        "agent": cfg.agent_name,
        "result": truncated,
        "resolution": truncated,
    });
    let _ = client
        .post(format!("{}/api/item/{item_id}/complete", cfg.acc_url))
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
}

async fn post_fail(cfg: &Config, client: &reqwest::Client, item_id: &str, reason: &str) {
    let body = serde_json::json!({
        "agent": cfg.agent_name,
        "reason": &reason[..reason.len().min(2000)],
    });
    let _ = client
        .post(format!("{}/api/item/{item_id}/fail", cfg.acc_url))
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
}

fn log(cfg: &Config, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{}] [hermes] {msg}", cfg.agent_name);
    eprintln!("{line}");
    let log_path = cfg.log_file("hermes-driver");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client")
}

// Helper trait for setting optional env vars on Command
trait CommandExt {
    fn env_opt(self, key: &str, val: Option<&str>) -> Self;
}

impl CommandExt for &mut Command {
    fn env_opt(self, key: &str, val: Option<&str>) -> Self {
        if let Some(v) = val {
            self.env(key, v);
        }
        self
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_mock::{HubMock, HubState};
    use serde_json::json;
    use std::path::PathBuf;

    fn test_cfg(url: &str) -> Config {
        Config {
            acc_dir: PathBuf::from("/tmp"),
            acc_url: url.to_string(),
            acc_token: "test-tok".to_string(),
            agent_name: "natasha".to_string(),
            agentbus_token: String::new(),
            pair_programming: false,
        }
    }

    // ── Pure unit tests ───────────────────────────────────────────────────────

    #[test]
    fn test_extract_session_id_found() {
        let output = "session_id: 20260417_153022_abc123 at turn 5";
        assert_eq!(
            extract_session_id(output),
            Some("20260417_153022_abc123".into())
        );
    }

    #[test]
    fn test_extract_session_id_none() {
        let output = "normal hermes output without session info";
        assert_eq!(extract_session_id(output), None);
    }

    // ── fetch_hermes_item hub mock tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_hermes_item_returns_hermes_tagged() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-h1", "status": "pending", "assignee": "all",
                   "tags": ["hermes"], "preferred_executor": ""}),
        ]).await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_some(), "hermes-tagged item should be returned");
        assert_eq!(item.unwrap()["id"], "wq-h1");
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_returns_gpu_tagged() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-g1", "status": "pending", "assignee": "all",
                   "tags": ["gpu", "render"], "preferred_executor": ""}),
        ]).await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_some());
        assert_eq!(item.unwrap()["id"], "wq-g1");
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_returns_preferred_executor_hermes() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-pe", "status": "pending", "assignee": "all",
                   "tags": [], "preferred_executor": "hermes"}),
        ]).await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_some());
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_skips_claude_cli() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-c1", "status": "pending", "assignee": "all",
                   "tags": ["docs", "claude_cli"], "preferred_executor": "claude_cli"}),
        ]).await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_none(), "claude_cli item should be skipped");
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_returns_coding_task() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-code1", "status": "pending", "assignee": "all",
                   "tags": ["code", "reasoning"], "preferred_executor": ""}),
        ]).await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_some(), "coding task should be accepted by hermes");
        assert_eq!(item.unwrap()["id"], "wq-code1");
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_skips_non_pending() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-ip", "status": "in-progress", "assignee": "all",
                   "tags": ["hermes"], "preferred_executor": ""}),
        ]).await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_none(), "non-pending item must be skipped");
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_skips_wrong_assignee() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-other", "status": "pending", "assignee": "boris",
                   "tags": ["hermes"], "preferred_executor": ""}),
        ]).await;
        let client = reqwest::Client::new();
        // cfg.agent_name = "natasha", item assigned to "boris"
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_none(), "item assigned to another agent must be skipped");
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_accepts_own_assignee() {
        let mock = HubMock::with_queue(vec![
            json!({"id": "wq-mine", "status": "pending", "assignee": "natasha",
                   "tags": ["hermes"], "preferred_executor": ""}),
        ]).await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_some());
        assert_eq!(item.unwrap()["id"], "wq-mine");
    }

    #[tokio::test]
    async fn test_fetch_hermes_item_empty_queue() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        let item = fetch_hermes_item(&test_cfg(&mock.url), &client).await;
        assert!(item.is_none());
    }

    // ── api_claim ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_api_claim_success_returns_true() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        assert!(api_claim(&test_cfg(&mock.url), &client, "wq-111").await);
    }

    #[tokio::test]
    async fn test_api_claim_conflict_returns_false() {
        let mock = HubMock::with_state(HubState { item_claim_status: 409, ..Default::default() }).await;
        let client = reqwest::Client::new();
        assert!(!api_claim(&test_cfg(&mock.url), &client, "wq-222").await);
    }

    // ── fire-and-forget helpers ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_post_heartbeat_no_panic() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        post_heartbeat(&test_cfg(&mock.url), &client, "hermes-test").await;
    }

    #[tokio::test]
    async fn test_post_complete_no_panic() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        post_complete(&test_cfg(&mock.url), &client, "wq-333", "output text").await;
    }

    #[tokio::test]
    async fn test_post_fail_no_panic() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        post_fail(&test_cfg(&mock.url), &client, "wq-444", "timeout").await;
    }
}
