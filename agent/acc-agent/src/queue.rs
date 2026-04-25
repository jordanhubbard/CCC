//! ACC queue worker daemon.
//!
//! Replaces queue-worker.py. Polls /api/queue, claims and executes tasks:
//!   - claude tasks: runs `claude -p <prompt>` in a task workspace
//!   - hermes tasks: delegates to `acc-agent hermes --item <id> --query ...`
//!   - beads tasks: calls `bd update --claim` / `bd close` (bd must be in PATH)
//!
//! Workspace lifecycle per task:
//!   1. Init: git clone repo → /tmp/acc-workspace-<id>/  → mirror to AgentFS
//!   2. Execute: subprocess runs with CWD = local workspace
//!   3. Finalize: git add/commit/push to task/<id> branch
//!   4. Abandon: on failure, preserve workspace in AgentFS for debugging

use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;

use acc_client::Client;
use acc_model::HeartbeatRequest;
use crate::config::Config;
use crate::peers;

const POLL_INTERVAL_IDLE: Duration = Duration::from_secs(60);
const POLL_INTERVAL_BUSY: Duration = Duration::from_secs(5);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(25 * 60);
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(7200);
const WORK_SIGNAL_CHECK: Duration = Duration::from_secs(1);

const HERMES_TAGS: &[&str] = &["hermes", "gpu", "render", "simulation", "omniverse", "isaaclab"];

pub async fn run(args: &[String]) {
    let once = args.iter().any(|a| a == "--once");

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[queue] config error: {e}");
            std::process::exit(1);
        }
    };

    if cfg.agent_name.is_empty() {
        eprintln!("[queue] AGENT_NAME not set");
        std::process::exit(1);
    }

    let _ = std::fs::create_dir_all(cfg.acc_dir.join("logs"));
    let _ = std::fs::create_dir_all(cfg.acc_dir.join("task-workspaces"));

    let caps = detect_capabilities();
    log(&cfg, &format!(
        "starting (agent={}, hub={}) caps={:?}",
        cfg.agent_name, cfg.acc_url, caps
    ));

    let client = build_client(&cfg);
    post_heartbeat(&cfg, &client, "queue-worker starting").await;

    let mut poll_interval = POLL_INTERVAL_IDLE;

    loop {
        if is_quenched(&cfg) {
            log(&cfg, "quenched — skipping cycle");
            sleep(POLL_INTERVAL_IDLE).await;
            continue;
        }

        post_heartbeat(&cfg, &client, "idle").await;

        // Consume work-signal if present
        let _ = std::fs::remove_file(cfg.work_signal_file());

        let items = match fetch_queue(&cfg, &client).await {
            Ok(v) => v,
            Err(e) => {
                log(&cfg, &format!("queue fetch failed: {e}"));
                sleep(POLL_INTERVAL_IDLE).await;
                continue;
            }
        };

        let online_peers = peers::list_peers(&cfg, &client).await;
        let item = select_item(&items, &cfg.agent_name, &caps, &online_peers);
        if let Some(item) = item {
            let item_id = item["id"].as_str().unwrap_or("").to_string();
            let title = item["title"].as_str().unwrap_or("?");
            log(&cfg, &format!("claiming [{id}] {title}", id = &item_id, title = &title[..title.len().min(60)]));

            if !claim_item(&cfg, &client, &item_id).await {
                log(&cfg, &format!("[{item_id}] claim rejected"));
                sleep(POLL_INTERVAL_BUSY).await;
                poll_interval = POLL_INTERVAL_IDLE;
                if once { break; }
                continue;
            }

            execute_item(&cfg, &client, &item).await;
            poll_interval = POLL_INTERVAL_BUSY;
        } else {
            log(&cfg, "no claimable items — sleeping");
            if once { break; }
            // Interruptible idle sleep
            let deadline = tokio::time::Instant::now() + poll_interval;
            while tokio::time::Instant::now() < deadline {
                if cfg.work_signal_file().exists() {
                    break;
                }
                sleep(WORK_SIGNAL_CHECK).await;
            }
            poll_interval = POLL_INTERVAL_IDLE;
        }

        if once { break; }
    }
}

async fn execute_item(cfg: &Config, client: &Client, item: &serde_json::Value) {
    let item_id = item["id"].as_str().unwrap_or("").to_string();

    // Init workspace
    let (workspace_local, workspace_agentfs) = match init_workspace(cfg, item).await {
        Ok(paths) => paths,
        Err(e) => {
            log(cfg, &format!("[{item_id}] workspace init failed: {e}"));
            post_fail(cfg, client, &item_id, &format!("workspace init: {e}")).await;
            return;
        }
    };

    let task_env = build_task_env(&item_id, &workspace_local, &workspace_agentfs);

    post_comment(cfg, client, &item_id, &format!(
        "{} starting — workspace {}",
        cfg.agent_name,
        workspace_local.display()
    )).await;

    // Execute
    let (output, exit_code) = if is_hermes_task(item) {
        log(cfg, &format!("[{item_id}] routing to acc-agent hermes"));
        run_hermes_driver(cfg, item, &item_id, &task_env, &workspace_local).await
    } else {
        let prompt = build_prompt(item, &workspace_local);
        log(cfg, &format!("[{item_id}] running claude"));
        run_claude(cfg, &prompt, &item_id, &task_env, &workspace_local).await
    };

    // Finalize
    if exit_code == 0 {
        let git_result = finalize_workspace(cfg, &item_id, &workspace_local, &workspace_agentfs, &output).await;
        let full_result = if git_result.is_empty() {
            output
        } else {
            format!("{output}\n\n---\ngit: {git_result}")
        };
        post_complete(cfg, client, &item_id, &full_result).await;
        log(cfg, &format!("[{item_id}] completed OK"));
    } else {
        abandon_workspace(cfg, &item_id, &workspace_local, &workspace_agentfs).await;
        post_fail(cfg, client, &item_id, &format!("exit_code={exit_code}\n{}", &output[..output.len().min(1000)])).await;
        log(cfg, &format!("[{item_id}] failed (exit={exit_code})"));
    }
}

// ── Workspace ─────────────────────────────────────────────────────────────────

async fn init_workspace(cfg: &Config, item: &serde_json::Value) -> Result<(PathBuf, String), String> {
    let item_id = item["id"].as_str().unwrap_or("");
    let workspace_local = std::env::temp_dir().join(format!("acc-workspace-{item_id}"));
    let _ = std::fs::create_dir_all(&workspace_local);

    let agentfs_path = agentfs_workspace_path(cfg, item_id);

    // Try AgentFS mirror first
    if !agentfs_path.is_empty() {
        if let Ok(true) = mc_mirror_pull(cfg, item_id, &agentfs_path, &workspace_local).await {
            // mirrored successfully
            let repo_url = repo_url_from_item(item);
            let branch = item["branch"].as_str().unwrap_or("main");
            mc_mirror_push(cfg, item_id, &workspace_local, &agentfs_path).await;
            write_agentfs_meta(cfg, item, &agentfs_path, &repo_url, branch, &workspace_local).await;
            return Ok((workspace_local, agentfs_path));
        }
    }

    // Fall back to git clone
    let repo_url = repo_url_from_item(item);
    let branch = item["branch"].as_str().unwrap_or("main");
    if !repo_url.is_empty() {
        log(cfg, &format!("[{item_id}] cloning {repo_url} ({branch})"));
        git_clone(cfg, item_id, &repo_url, branch, &workspace_local).await;
    }

    if !agentfs_path.is_empty() {
        mc_mirror_push(cfg, item_id, &workspace_local, &agentfs_path).await;
        write_agentfs_meta(cfg, item, &agentfs_path, &repo_url, branch, &workspace_local).await;
    }

    Ok((workspace_local, agentfs_path))
}

async fn git_clone(cfg: &Config, item_id: &str, repo_url: &str, branch: &str, dest: &Path) {
    // Try with branch first, fall back to default branch
    let r = Command::new("git")
        .args(["clone", "--depth=1", "--branch", branch, repo_url, dest.to_str().unwrap_or(".")])
        .output()
        .await;
    if r.map(|o| o.status.success()).unwrap_or(false) {
        return;
    }
    // Default branch
    let r = Command::new("git")
        .args(["clone", "--depth=1", repo_url, dest.to_str().unwrap_or(".")])
        .output()
        .await;
    match r {
        Ok(o) if o.status.success() => {
            log(cfg, &format!("[{item_id}] cloned (default branch)"));
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            log(cfg, &format!("[{item_id}] WARNING: clone failed: {err}"));
        }
        Err(e) => log(cfg, &format!("[{item_id}] WARNING: clone error: {e}")),
    }
}

async fn finalize_workspace(
    cfg: &Config,
    item_id: &str,
    local: &PathBuf,
    agentfs: &str,
    task_output: &str,
) -> String {
    if !local.exists() {
        return String::new();
    }
    if !agentfs.is_empty() {
        mc_mirror_push(cfg, item_id, local, agentfs).await;
    }
    if !local.join(".git").exists() {
        cleanup_workspace(cfg, item_id, local).await;
        return String::new();
    }
    let result = git_push_once(cfg, item_id, local, task_output).await;
    cleanup_workspace(cfg, item_id, local).await;
    result
}

async fn abandon_workspace(cfg: &Config, item_id: &str, local: &PathBuf, agentfs: &str) {
    if !agentfs.is_empty() {
        log(cfg, &format!("[{item_id}] preserving failed workspace in AgentFS"));
        mc_mirror_push(cfg, item_id, local, agentfs).await;
    }
    cleanup_workspace(cfg, item_id, local).await;
}

async fn cleanup_workspace(cfg: &Config, item_id: &str, local: &PathBuf) {
    if let Err(e) = std::fs::remove_dir_all(local) {
        log(cfg, &format!("[{item_id}] cleanup warning: {e}"));
    }
}

async fn git_push_once(cfg: &Config, item_id: &str, workspace: &PathBuf, task_output: &str) -> String {
    let cwd = workspace.to_str().unwrap_or(".");

    // Check if anything to commit
    let status = Command::new("git")
        .args(["-C", cwd, "status", "--porcelain"])
        .output()
        .await;
    if status.map(|o| o.stdout.is_empty()).unwrap_or(true) {
        return "workspace clean — no changes to push".into();
    }

    let task_branch = format!("task/{item_id}");

    // Create task branch
    match Command::new("git").args(["-C", cwd, "checkout", "-b", &task_branch]).output().await {
        Ok(o) if !o.status.success() => {
            log(cfg, &format!("[{item_id}] git checkout -b failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
        }
        Err(e) => log(cfg, &format!("[{item_id}] git checkout -b spawn failed: {e}")),
        _ => {}
    }

    // Stage all
    match Command::new("git").args(["-C", cwd, "add", "-A"]).output().await {
        Ok(o) if !o.status.success() => {
            log(cfg, &format!("[{item_id}] git add failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
            return "git add failed".into();
        }
        Err(e) => {
            log(cfg, &format!("[{item_id}] git add spawn failed: {e}"));
            return "git add failed".into();
        }
        _ => {}
    }

    // Commit
    let commit_msg = format!(
        "task({item_id}): complete\n\nAgent: {}\n\n{}",
        cfg.agent_name,
        &task_output[..task_output.len().min(500)]
    );
    match Command::new("git")
        .args([
            "-C", cwd,
            "-c", &format!("user.email={}@acc", cfg.agent_name),
            "-c", &format!("user.name={}", cfg.agent_name),
            "commit", "-m", &commit_msg,
        ])
        .output()
        .await
    {
        Ok(o) if !o.status.success() => {
            log(cfg, &format!("[{item_id}] git commit failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
        }
        Err(e) => log(cfg, &format!("[{item_id}] git commit spawn failed: {e}")),
        _ => {}
    }

    let sha = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--short", "HEAD"])
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    // Rewrite HTTPS to SSH if deploy key is available
    let deploy_key = dirs_home().join(".ssh").join("ccc-deploy-key");
    if deploy_key.exists() {
        let remote = Command::new("git")
            .args(["-C", cwd, "remote", "get-url", "origin"])
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if remote.starts_with("https://github.com/") {
            let ssh_url = remote.replacen("https://github.com/", "git@github.com:", 1);
            match Command::new("git")
                .args(["-C", cwd, "remote", "set-url", "origin", &ssh_url])
                .output()
                .await
            {
                Ok(o) if !o.status.success() => {
                    log(cfg, &format!("[{item_id}] git remote set-url failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
                }
                Err(e) => log(cfg, &format!("[{item_id}] git remote set-url spawn failed: {e}")),
                _ => {}
            }
        }
    }

    // Push (force-with-lease first, then set-upstream)
    let push = Command::new("git")
        .args(["-C", cwd, "push", "--force-with-lease", "origin", &task_branch])
        .output()
        .await;
    let push_err = match &push {
        Ok(o) if !o.status.success() => Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Ok(_) => None,
        Err(e) => Some(format!("spawn failed: {e}")),
    };
    if push.as_ref().map(|o| o.status.success()).unwrap_or(false) {
        return format!("pushed to {task_branch} @ {sha}");
    }
    if let Some(err) = push_err {
        log(cfg, &format!("[{item_id}] git push --force-with-lease failed: {err}"));
    }

    let push2 = Command::new("git")
        .args(["-C", cwd, "push", "--set-upstream", "origin", &task_branch])
        .output()
        .await;
    match &push2 {
        Ok(o) if !o.status.success() => {
            log(cfg, &format!("[{item_id}] git push --set-upstream failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
        }
        Err(e) => log(cfg, &format!("[{item_id}] git push --set-upstream spawn failed: {e}")),
        _ => {}
    }
    if push2.map(|o| o.status.success()).unwrap_or(false) {
        format!("pushed to {task_branch} @ {sha}")
    } else {
        format!("committed @ {sha} — push failed")
    }
}

fn agentfs_workspace_path(_cfg: &Config, item_id: &str) -> String {
    let endpoint = std::env::var("MINIO_ENDPOINT").unwrap_or_default();
    if endpoint.is_empty() {
        return String::new();
    }
    let alias = std::env::var("MINIO_ALIAS").unwrap_or_else(|_| "ccc-hub".into());
    let bucket = std::env::var("MINIO_BUCKET").unwrap_or_else(|_| "agents".into());
    format!("{alias}/{bucket}/tasks/{item_id}/workspace")
}

async fn mc_mirror_push(cfg: &Config, item_id: &str, local: &PathBuf, agentfs: &str) {
    let local_str = format!("{}/", local.display());
    let r = Command::new("mc")
        .args(["mirror", "--overwrite", "--quiet", &local_str, agentfs])
        .output()
        .await;
    match r {
        Ok(o) if o.status.success() => log(cfg, &format!("[{item_id}] workspace → AgentFS")),
        Ok(o) => log(cfg, &format!("[{item_id}] AgentFS push warning: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => log(cfg, &format!("[{item_id}] AgentFS push error: {e}")),
    }
}

async fn mc_mirror_pull(_cfg: &Config, _item_id: &str, agentfs: &str, local: &PathBuf) -> Result<bool, String> {
    if which_bin("mc").is_none() { return Ok(false); }
    let local_str = format!("{}/", local.display());
    let r = Command::new("mc")
        .args(["mirror", "--overwrite", "--quiet", agentfs, &local_str])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    Ok(r.status.success())
}

async fn write_agentfs_meta(
    _cfg: &Config, item: &serde_json::Value, agentfs: &str,
    repo_url: &str, branch: &str, workspace: &PathBuf,
) {
    let sha = if workspace.join(".git").exists() {
        Command::new("git")
            .args(["-C", workspace.to_str().unwrap_or("."), "rev-parse", "HEAD"])
            .output()
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let parts: Vec<&str> = agentfs.splitn(3, '/').collect();
    let (mc_alias, bucket) = (parts.first().copied().unwrap_or("ccc-hub"), parts.get(1).copied().unwrap_or("agents"));

    let meta = serde_json::json!({
        "task_id":      item["id"],
        "title":        item["title"],
        "repo":         repo_url,
        "branch":       branch,
        "sha":          sha,
        "initiated_at": chrono::Utc::now().to_rfc3339(),
        "agent":        std::env::var("AGENT_NAME").unwrap_or_default(),
    });
    let meta_str = serde_json::to_string(&meta).unwrap_or_default();
    let item_id = item["id"].as_str().unwrap_or("");
    // Pipe meta JSON via echo to mc
    let _ = Command::new("sh")
        .args(["-c", &format!(
            "echo {} | mc pipe {mc_alias}/{bucket}/tasks/{item_id}/meta.json",
            shlex_quote(&meta_str)
        )])
        .output()
        .await;
}

// ── Task execution ─────────────────────────────────────────────────────────────

async fn run_claude(
    cfg: &Config,
    prompt: &str,
    item_id: &str,
    task_env: &[(String, String)],
    workspace: &PathBuf,
) -> (String, i32) {
    let claude_bin = which_bin("claude").unwrap_or_else(|| "claude".into());

    // Keepalive task
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let ka_cfg = cfg.clone();
    let ka_item = item_id.to_string();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(KEEPALIVE_INTERVAL);
        interval.tick().await;
        tokio::select! {
            _ = async {
                loop {
                    interval.tick().await;
                    let client = build_client(&ka_cfg);
                    post_keepalive(&ka_cfg, &client, &ka_item, "claude still working").await;
                }
            } => {}
            _ = stop_rx => {}
        }
    });

    let result = tokio::time::timeout(
        CLAUDE_TIMEOUT,
        Command::new(&claude_bin)
            .arg("-p")
            .arg(prompt)
            .envs(task_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(workspace)
            .output(),
    )
    .await;

    let _ = stop_tx.send(());

    match result {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let output = if stdout.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                stdout.trim().to_string()
            };
            let code = out.status.code().unwrap_or(1);
            (output, code)
        }
        Ok(Err(e)) => (format!("ERROR: {e}"), 1),
        Err(_) => (format!("[timed out after {}s]", CLAUDE_TIMEOUT.as_secs()), 124),
    }
}

async fn run_hermes_driver(
    _cfg: &Config,
    item: &serde_json::Value,
    item_id: &str,
    task_env: &[(String, String)],
    workspace: &PathBuf,
) -> (String, i32) {
    let acc_agent = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("acc-agent"));
    let query = format!(
        "{}\n\n{}",
        item["title"].as_str().unwrap_or(""),
        item["description"].as_str().unwrap_or("")
    );
    let r = tokio::time::timeout(
        Duration::from_secs(86400),
        Command::new(&acc_agent)
            .args(["hermes", "--item", item_id, "--query", &query])
            .envs(task_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(workspace)
            .output(),
    )
    .await;

    match r {
        Ok(Ok(out)) => {
            let output = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (output, out.status.code().unwrap_or(1))
        }
        Ok(Err(e)) => (format!("ERROR: {e}"), 1),
        Err(_) => ("[hermes-driver timed out]".into(), 124),
    }
}

// ── Queue API ──────────────────────────────────────────────────────────────────

async fn fetch_queue(_cfg: &Config, client: &Client) -> Result<Vec<serde_json::Value>, String> {
    let items = client.queue().list().await.map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|i| serde_json::to_value(i).unwrap_or(serde_json::Value::Null))
        .collect())
}

fn select_item<'a>(items: &'a [serde_json::Value], agent_name: &str, caps: &[String], online_peers: &[String]) -> Option<&'a serde_json::Value> {
    let priority_order = |p: &str| match p {
        "urgent" => 0u8,
        "high" => 1,
        "normal" | "medium" => 2,
        "low" => 3,
        _ => 4,
    };

    let mut candidates: Vec<&serde_json::Value> = items
        .iter()
        .filter(|item| {
            if item["status"].as_str() != Some("pending") { return false; }
            let assignee = item["assignee"].as_str().unwrap_or("");
            if assignee == "jkh" { return false; }
            if !assignee.is_empty() && assignee != "all" && assignee != agent_name {
                return false;
            }
            // Hard capability gate
            if let Some(required) = item["required_executors"].as_array() {
                if !required.is_empty() {
                    let req_set: Vec<&str> = required.iter()
                        .filter_map(|v| v.as_str())
                        .collect();
                    let has_cap = req_set.iter().any(|r| caps.iter().any(|c| c == r));
                    if !has_cap { return false; }
                }
            }
            // Collaboration gate: if a specific peer is preferred and online, let them handle it
            let preferred = item["preferred_executor"].as_str().unwrap_or("");
            if !preferred.is_empty()
                && preferred != agent_name
                && online_peers.iter().any(|p| p == preferred)
            {
                return false;
            }
            true
        })
        .collect();

    candidates.sort_by_key(|item| {
        let p = item["priority"].as_str().unwrap_or("normal");
        let t = item["created"].as_str().unwrap_or("");
        (priority_order(p), t.to_string())
    });

    candidates.into_iter().next()
}

async fn claim_item(cfg: &Config, client: &Client, item_id: &str) -> bool {
    client
        .items()
        .claim(item_id, &cfg.agent_name, Some("claiming"))
        .await
        .is_ok()
}

async fn post_complete(cfg: &Config, client: &Client, item_id: &str, result: &str) {
    let truncated = &result[..result.len().min(4000)];
    let _ = client
        .items()
        .complete(item_id, &cfg.agent_name, Some(truncated), Some(truncated))
        .await;
}

async fn post_fail(cfg: &Config, client: &Client, item_id: &str, reason: &str) {
    let truncated = &reason[..reason.len().min(2000)];
    let _ = client.items().fail(item_id, &cfg.agent_name, truncated).await;
}

async fn post_comment(cfg: &Config, client: &Client, item_id: &str, comment: &str) {
    let _ = client
        .items()
        .comment(item_id, &cfg.agent_name, comment)
        .await;
}

async fn post_heartbeat(cfg: &Config, client: &Client, note: &str) {
    let req = HeartbeatRequest {
        ts: Some(chrono::Utc::now()),
        status: Some("ok".into()),
        note: Some(note.into()),
        host: Some(cfg.host.clone()),
        ssh_user: Some(cfg.ssh_user.clone()),
        ssh_host: Some(cfg.ssh_host.clone()),
        ssh_port: Some(cfg.ssh_port as u64),
    };
    let _ = client.items().heartbeat(&cfg.agent_name, &req).await;
}

async fn post_keepalive(cfg: &Config, client: &Client, item_id: &str, note: &str) {
    let _ = client
        .items()
        .keepalive(item_id, &cfg.agent_name, Some(note))
        .await;
}

// ── Helpers ────────────────────────────────────────────────────────────────────

fn is_quenched(cfg: &Config) -> bool {
    let quench_file = cfg.quench_file();
    let content = std::fs::read_to_string(quench_file).unwrap_or_default();
    if content.is_empty() { return false; }
    if let Ok(until) = chrono::DateTime::parse_from_rfc3339(content.trim()) {
        return chrono::Utc::now() < until;
    }
    false
}

fn is_hermes_task(item: &serde_json::Value) -> bool {
    let tags: Vec<&str> = item["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let preferred = item["preferred_executor"].as_str().unwrap_or("");
    tags.iter().any(|t| HERMES_TAGS.contains(t)) || HERMES_TAGS.contains(&preferred)
}

fn detect_capabilities() -> Vec<String> {
    let from_env = std::env::var("AGENT_CAPABILITIES").unwrap_or_default();
    if !from_env.is_empty() {
        return from_env.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    }
    let mut caps = Vec::new();
    if which_bin("claude").is_some() {
        caps.push("claude_cli".into());
        caps.push("claude_sdk".into());
    }
    if which_bin("hermes").is_some() {
        caps.push("hermes".into());
    }
    if std::env::var("NVIDIA_API_KEY").is_ok() || std::env::var("ANTHROPIC_API_KEY").is_ok() {
        caps.push("inference_key".into());
    }
    if Path::new("/proc/driver/nvidia").exists() || which_bin("nvidia-smi").is_some() {
        caps.push("gpu".into());
    }
    caps
}

fn which_bin(name: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok().and_then(|path_var| {
        path_var.split(':').find_map(|dir| {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.exists() { Some(candidate) } else { None }
        })
    })
}

fn repo_url_from_item(item: &serde_json::Value) -> String {
    item["repo"].as_str()
        .or_else(|| item["repository"].as_str())
        .unwrap_or("")
        .to_string()
}

fn build_task_env(item_id: &str, local: &PathBuf, agentfs: &str) -> Vec<(String, String)> {
    vec![
        ("TASK_ID".into(), item_id.to_string()),
        ("TASK_WORKSPACE_LOCAL".into(), local.to_str().unwrap_or(".").to_string()),
        ("TASK_WORKSPACE_AGENTFS".into(), agentfs.to_string()),
        ("TASK_BRANCH".into(), format!("task/{item_id}")),
    ]
}

fn build_prompt(item: &serde_json::Value, workspace: &PathBuf) -> String {
    let mut parts = vec![
        format!("# Queue Item: {}", item["id"].as_str().unwrap_or("")),
        format!("**Title:** {}", item["title"].as_str().unwrap_or("")),
        format!("**Priority:** {}", item["priority"].as_str().unwrap_or("normal")),
        String::new(),
        "## Task Workspace".into(),
        format!("Your working directory is: `{}`", workspace.display()),
        "All file edits must happen inside this directory.".into(),
        "Do NOT run `git commit` or `git push` — the queue-worker handles that.".into(),
        String::new(),
        "## Description".into(),
        item["description"].as_str().unwrap_or("(no description)").to_string(),
    ];
    if let Some(notes) = item["notes"].as_str() {
        if !notes.is_empty() {
            parts.push(String::new());
            parts.push("## Notes".into());
            parts.push(notes.to_string());
        }
    }
    parts.push(String::new());
    parts.push("---".into());
    parts.push("Complete the work item above. When done, summarize what you did.".into());
    parts.join("\n")
}

fn shlex_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn dirs_home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
}

fn log_tracing(cfg: &Config, msg: &str) {
    tracing::info!(component = "queue", agent = %cfg.agent_name, "{msg}");
}
fn log(cfg: &Config, msg: &str) {
    log_tracing(cfg, msg);
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{}] [queue] {msg}", cfg.agent_name);
    eprintln!("{line}");
    let log_path = cfg.log_file("queue-worker");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

fn build_client(cfg: &Config) -> Client {
    Client::new(&cfg.acc_url, &cfg.acc_token).expect("failed to build HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_select_item_by_priority() {
        let items = vec![
            json!({"id": "1", "status": "pending", "assignee": "all", "priority": "low", "created": "2026-01-02T00:00:00Z"}),
            json!({"id": "2", "status": "pending", "assignee": "all", "priority": "urgent", "created": "2026-01-03T00:00:00Z"}),
            json!({"id": "3", "status": "pending", "assignee": "all", "priority": "normal", "created": "2026-01-01T00:00:00Z"}),
        ];
        let caps = vec!["claude_cli".into()];
        let selected = select_item(&items, "boris", &caps, &[]).unwrap();
        assert_eq!(selected["id"], "2"); // urgent first
    }

    #[test]
    fn test_select_item_skips_wrong_assignee() {
        let items = vec![
            json!({"id": "1", "status": "pending", "assignee": "natasha", "priority": "normal", "created": ""}),
            json!({"id": "2", "status": "pending", "assignee": "all", "priority": "normal", "created": ""}),
        ];
        let caps = vec![];
        let selected = select_item(&items, "boris", &caps, &[]).unwrap();
        assert_eq!(selected["id"], "2");
    }

    #[test]
    fn test_select_item_skips_human_reserved() {
        let items = vec![
            json!({"id": "1", "status": "pending", "assignee": "jkh", "priority": "normal", "created": ""}),
        ];
        let selected = select_item(&items, "boris", &[], &[]);
        assert!(selected.is_none());
    }

    #[test]
    fn test_select_item_capability_gate() {
        let items = vec![
            json!({"id": "1", "status": "pending", "assignee": "all", "priority": "normal",
                   "required_executors": ["gpu"], "created": ""}),
        ];
        // No gpu cap
        let no_gpu = select_item(&items, "boris", &["claude_cli".into()], &[]);
        assert!(no_gpu.is_none());

        // With gpu cap
        let with_gpu = select_item(&items, "boris", &["gpu".into()], &[]);
        assert!(with_gpu.is_some());
    }

    #[test]
    fn test_select_item_skips_when_preferred_peer_online() {
        let items = vec![
            json!({"id": "1", "status": "pending", "assignee": "all", "priority": "normal",
                   "preferred_executor": "natasha", "created": ""}),
        ];
        // natasha is online — boris should skip this item
        let selected = select_item(&items, "boris", &[], &["natasha".to_string()]);
        assert!(selected.is_none(), "must skip task when preferred peer is online");
    }

    #[test]
    fn test_select_item_claims_when_preferred_peer_offline() {
        let items = vec![
            json!({"id": "1", "status": "pending", "assignee": "all", "priority": "normal",
                   "preferred_executor": "natasha", "created": ""}),
        ];
        // natasha is NOT in online_peers — boris should take it
        let selected = select_item(&items, "boris", &[], &[]);
        assert!(selected.is_some(), "must claim task when preferred peer is offline");
    }

    #[test]
    fn test_select_item_self_preferred_claims() {
        let items = vec![
            json!({"id": "1", "status": "pending", "assignee": "all", "priority": "normal",
                   "preferred_executor": "boris", "created": ""}),
        ];
        // I am the preferred executor — I should claim it
        let selected = select_item(&items, "boris", &[], &["natasha".to_string()]);
        assert!(selected.is_some(), "must claim task when self is preferred executor");
    }

    #[test]
    fn test_is_hermes_task() {
        let item = json!({"tags": ["gpu", "render"], "preferred_executor": ""});
        assert!(is_hermes_task(&item));

        let item2 = json!({"tags": ["docs"], "preferred_executor": "claude_cli"});
        assert!(!is_hermes_task(&item2));
    }

    #[test]
    fn test_build_task_env() {
        let env = build_task_env("wq-123", &PathBuf::from("/tmp/ws"), "ccc-hub/agents/tasks/wq-123/workspace");
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"TASK_ID"));
        assert!(keys.contains(&"TASK_WORKSPACE_LOCAL"));
        assert!(keys.contains(&"TASK_BRANCH"));
    }

    #[test]
    fn test_is_quenched_no_file() {
        // Config without a real acc_dir — quench file won't exist
        let cfg = Config {
            acc_dir: PathBuf::from("/nonexistent"),
            acc_url: "http://localhost".into(),
            acc_token: "tok".into(),
            agent_name: "test".into(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: String::new(),
            ssh_user: "testuser".into(),
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
        };
        assert!(!is_quenched(&cfg));
    }

    // ── Hub mock HTTP tests ───────────────────────────────────────────────────

    fn mock_cfg(url: &str) -> Config {
        Config {
            acc_dir: PathBuf::from("/tmp"),
            acc_url: url.to_string(),
            acc_token: "tok".to_string(),
            agent_name: "boris".to_string(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: String::new(),
            ssh_user: "testuser".into(),
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
        }
    }

    #[tokio::test]
    async fn test_fetch_queue_returns_items() {
        let mock = crate::hub_mock::HubMock::with_queue(vec![
            json!({"id": "wq-1", "title": "Item 1", "status": "pending",
                   "assignee": "all", "priority": "normal", "created": "2026-01-01T00:00:00Z"}),
            json!({"id": "wq-2", "title": "Item 2", "status": "pending",
                   "assignee": "all", "priority": "urgent", "created": "2026-01-02T00:00:00Z"}),
        ]).await;
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        let items = fetch_queue(&mock_cfg(&mock.url), &client).await.unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i["id"] == "wq-1"));
    }

    #[tokio::test]
    async fn test_fetch_queue_empty_hub() {
        let mock = crate::hub_mock::HubMock::new().await;
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        let items = fetch_queue(&mock_cfg(&mock.url), &client).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_queue_hub_down_returns_err() {
        let cfg = mock_cfg("http://127.0.0.1:1"); // nothing listening on port 1
        let client = build_client(&cfg);
        let result = fetch_queue(&cfg, &client).await;
        assert!(result.is_err(), "unreachable hub must return Err");
    }

    #[tokio::test]
    async fn test_claim_item_success_returns_true() {
        let mock = crate::hub_mock::HubMock::new().await; // default 200
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        assert!(claim_item(&mock_cfg(&mock.url), &client, "wq-111").await);
    }

    #[tokio::test]
    async fn test_claim_item_conflict_returns_false() {
        use crate::hub_mock::HubState;
        let mock = crate::hub_mock::HubMock::with_state(
            HubState { item_claim_status: 409, ..Default::default() }
        ).await;
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        assert!(!claim_item(&mock_cfg(&mock.url), &client, "wq-222").await);
    }

    #[tokio::test]
    async fn test_post_heartbeat_does_not_panic() {
        // post_heartbeat is fire-and-forget; verify it completes without panic.
        let mock = crate::hub_mock::HubMock::new().await;
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        post_heartbeat(&mock_cfg(&mock.url), &client, "test note").await;
    }

    #[tokio::test]
    async fn test_post_complete_does_not_panic() {
        let mock = crate::hub_mock::HubMock::new().await;
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        post_complete(&mock_cfg(&mock.url), &client, "wq-333", "done").await;
    }

    #[tokio::test]
    async fn test_post_fail_does_not_panic() {
        let mock = crate::hub_mock::HubMock::new().await;
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        post_fail(&mock_cfg(&mock.url), &client, "wq-444", "timeout").await;
    }

    #[tokio::test]
    async fn test_select_item_prefers_urgent_from_fetched_queue() {
        // Integration: fetch returns items, select_item picks the highest priority.
        let mock = crate::hub_mock::HubMock::with_queue(vec![
            json!({"id": "low",    "status": "pending", "assignee": "all",
                   "priority": "low",    "created": "2026-01-01T00:00:00Z"}),
            json!({"id": "urgent", "status": "pending", "assignee": "all",
                   "priority": "urgent", "created": "2026-01-02T00:00:00Z"}),
        ]).await;
        let cfg_ = mock_cfg(&mock.url);
        let client = build_client(&cfg_);
        let items = fetch_queue(&mock_cfg(&mock.url), &client).await.unwrap();
        let caps = vec![];
        let selected = select_item(&items, "boris", &caps, &[]).unwrap();
        assert_eq!(selected["id"], "urgent");
    }
}
