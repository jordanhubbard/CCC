//! Fleet task worker — polls /api/tasks, claims atomically, executes in AgentFS workspace.
//!
//! Work tasks run claude; review tasks run claude with a structured review prompt;
//! phase_commit tasks run git to push approved work to a branch.
//! Multiple agents run this concurrently; the server's SQL atomic claim prevents double-work.

use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use serde_json::Value;
use crate::config::Config;
use crate::peers;

const POLL_IDLE: Duration = Duration::from_secs(30);
const POLL_BUSY: Duration = Duration::from_secs(5);
const WORK_TIMEOUT: Duration = Duration::from_secs(7200); // 2h per task

pub async fn run(args: &[String]) {
    let max_concurrent: usize = args.iter()
        .find(|a| a.starts_with("--max="))
        .and_then(|a| a[6..].parse().ok())
        .or_else(|| std::env::var("ACC_MAX_TASKS_PER_AGENT").ok().and_then(|v| v.parse().ok()))
        .unwrap_or(2);

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => { eprintln!("[tasks] config error: {e}"); std::process::exit(1); }
    };
    if cfg.agent_name.is_empty() {
        eprintln!("[tasks] AGENT_NAME not set"); std::process::exit(1);
    }

    let _ = std::fs::create_dir_all(cfg.acc_dir.join("logs"));
    log(&cfg, &format!("starting (agent={}, hub={}, max_concurrent={}, pair_programming={})",
        cfg.agent_name, cfg.acc_url, max_concurrent, cfg.pair_programming));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("http client");

    loop {
        if is_quenched(&cfg) {
            log(&cfg, "quenched — sleeping");
            sleep(POLL_IDLE).await;
            continue;
        }

        let active = count_active_tasks(&cfg, &client).await;
        if active >= max_concurrent {
            log(&cfg, &format!("at capacity ({}/{}), waiting", active, max_concurrent));
            sleep(POLL_IDLE).await;
            continue;
        }

        // Fetch online peers once per cycle (used by all three polls)
        let online_peers = peers::list_peers(&cfg, &client).await;
        let mut claimed = false;

        // ── Poll 1: work tasks ──────────────────────────────────────────────
        let fetch_limit = ((max_concurrent - active) * 5).max(10);
        match fetch_open_tasks(&cfg, &client, fetch_limit, "work").await {
            Err(e) => {
                log(&cfg, &format!("fetch failed: {e}"));
                sleep(POLL_IDLE).await;
                continue;
            }
            Ok(open_tasks) => {
                for task in &open_tasks {
                    let task_id = task["id"].as_str().unwrap_or("").to_string();
                    if task_id.is_empty() { continue; }

                    let preferred = task["metadata"]["preferred_executor"].as_str().unwrap_or("");
                    if !preferred.is_empty()
                        && preferred != cfg.agent_name.as_str()
                        && online_peers.iter().any(|p| p == preferred)
                    {
                        log(&cfg, &format!("skipping {task_id} — preferred by {preferred} (online)"));
                        continue;
                    }

                    match claim_task(&cfg, &client, &task_id).await {
                        Ok(claimed_task) => {
                            log(&cfg, &format!("claimed task {task_id}: {}", claimed_task["title"].as_str().unwrap_or("")));
                            let cfg2 = cfg.clone();
                            let client2 = client.clone();
                            let task2 = claimed_task.clone();
                            let peers2 = online_peers.clone();
                            tokio::spawn(async move {
                                execute_task(&cfg2, &client2, &task2, &peers2).await;
                            });
                            claimed = true;
                            break;
                        }
                        Err(409) | Err(423) => { /* already claimed or blocked, try next */ }
                        Err(429) => {
                            log(&cfg, "at capacity (server side)");
                            break;
                        }
                        Err(e) => {
                            log(&cfg, &format!("claim error {e} for {task_id}"));
                        }
                    }
                }
            }
        }

        // ── Poll 2: review tasks ────────────────────────────────────────────
        if !claimed {
            if let Ok(review_tasks) = fetch_open_tasks(&cfg, &client, 10, "review").await {
                for task in &review_tasks {
                    let task_id = task["id"].as_str().unwrap_or("").to_string();
                    if task_id.is_empty() { continue; }

                    let preferred = task["metadata"]["preferred_executor"].as_str().unwrap_or("");
                    if !preferred.is_empty()
                        && preferred != cfg.agent_name.as_str()
                        && online_peers.iter().any(|p| p == preferred)
                    {
                        continue;
                    }

                    match claim_task(&cfg, &client, &task_id).await {
                        Ok(claimed_task) => {
                            log(&cfg, &format!("claimed review {task_id}"));
                            let cfg2 = cfg.clone();
                            let client2 = client.clone();
                            let task2 = claimed_task.clone();
                            tokio::spawn(async move {
                                execute_review_task(&cfg2, &client2, &task2).await;
                            });
                            claimed = true;
                            break;
                        }
                        Err(409) | Err(423) => {}
                        Err(e) => { log(&cfg, &format!("review claim error {e} for {task_id}")); }
                    }
                }
            }
        }

        // ── Poll 3: phase_commit tasks ──────────────────────────────────────
        if !claimed {
            if let Ok(phase_tasks) = fetch_open_tasks(&cfg, &client, 5, "phase_commit").await {
                for task in &phase_tasks {
                    let task_id = task["id"].as_str().unwrap_or("").to_string();
                    if task_id.is_empty() { continue; }

                    match claim_task(&cfg, &client, &task_id).await {
                        Ok(claimed_task) => {
                            log(&cfg, &format!("claimed phase_commit {task_id}"));
                            let cfg2 = cfg.clone();
                            let client2 = client.clone();
                            let task2 = claimed_task.clone();
                            tokio::spawn(async move {
                                execute_phase_commit_task(&cfg2, &client2, &task2).await;
                            });
                            claimed = true;
                            break;
                        }
                        Err(409) | Err(423) => {}
                        Err(e) => { log(&cfg, &format!("phase_commit claim error {e} for {task_id}")); }
                    }
                }
            }
        }

        sleep(if claimed { POLL_BUSY } else { POLL_IDLE }).await;
    }
}

// ── Fetching / claiming ───────────────────────────────────────────────────────

async fn fetch_open_tasks(cfg: &Config, client: &reqwest::Client, limit: usize, task_type: &str) -> Result<Vec<Value>, String> {
    let url = format!("{}/api/tasks?status=open&task_type={}&limit={}", cfg.acc_url, task_type, limit.max(1));
    let resp = client.get(&url)
        .bearer_auth(&cfg.acc_token)
        .send().await
        .map_err(|e| e.to_string())?;
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(body["tasks"].as_array().cloned().unwrap_or_default())
}

async fn count_active_tasks(cfg: &Config, client: &reqwest::Client) -> usize {
    let url = format!("{}/api/tasks?status=claimed&agent={}", cfg.acc_url, cfg.agent_name);
    let Ok(resp) = client.get(&url).bearer_auth(&cfg.acc_token).send().await else { return 0; };
    let Ok(body): Result<Value, _> = resp.json().await else { return 0; };
    body["count"].as_u64().unwrap_or(0) as usize
}

async fn claim_task(cfg: &Config, client: &reqwest::Client, task_id: &str) -> Result<Value, u16> {
    let url = format!("{}/api/tasks/{}/claim", cfg.acc_url, task_id);
    let resp = client.put(&url)
        .bearer_auth(&cfg.acc_token)
        .json(&serde_json::json!({"agent": cfg.agent_name}))
        .send().await
        .map_err(|_| 500u16)?;
    let status = resp.status().as_u16();
    if status == 200 {
        let body: Value = resp.json().await.map_err(|_| 500u16)?;
        Ok(body["task"].clone())
    } else {
        Err(status)
    }
}

// ── Work task execution ───────────────────────────────────────────────────────

async fn execute_task(cfg: &Config, client: &reqwest::Client, task: &Value, online_peers: &[String]) {
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let title = task["title"].as_str().unwrap_or("(no title)");
    let project_id = task["project_id"].as_str().unwrap_or("");

    log(cfg, &format!("executing task {task_id}: {title}"));

    let workspace = resolve_workspace(cfg, project_id, task_id).await;
    let _ = std::fs::create_dir_all(&workspace);

    let ctx_path = workspace.join(".task-context.json");
    let _ = std::fs::write(&ctx_path, task.to_string());

    let result = run_task_subprocess(cfg, task, &workspace).await;

    match result {
        Ok(output) => {
            log(cfg, &format!("task {task_id} completed: {}", &output[..output.len().min(120)]));
            if cfg.pair_programming {
                submit_for_review(cfg, client, task, &output, online_peers).await;
            } else {
                complete_task(cfg, client, task_id, &output).await;
            }
        }
        Err(e) => {
            log(cfg, &format!("task {task_id} failed: {e}"));
            unclaim_task(cfg, client, task_id).await;
        }
    }
}

async fn run_task_subprocess(_cfg: &Config, task: &Value, workspace: &PathBuf) -> Result<String, String> {
    let description = task["description"].as_str().unwrap_or("");
    let title = task["title"].as_str().unwrap_or("(task)");

    let prompt = if description.is_empty() {
        title.to_string()
    } else {
        format!("{}\n\n{}", title, description)
    };

    let claude = which_claude();
    let mut cmd = Command::new(&claude);
    cmd.args(["-p", &prompt, "--dangerously-skip-permissions"])
       .current_dir(workspace)
       .kill_on_drop(true);

    let result = tokio::time::timeout(WORK_TIMEOUT, cmd.output()).await
        .map_err(|_| "task timed out".to_string())?
        .map_err(|e| format!("subprocess failed: {e}"))?;

    if result.status.success() {
        Ok(String::from_utf8_lossy(&result.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&result.stderr).to_string())
    }
}

// ── Pair programming: submit for review ──────────────────────────────────────

async fn submit_for_review(cfg: &Config, client: &reqwest::Client, task: &Value, output: &str, online_peers: &[String]) {
    let task_id = task["id"].as_str().unwrap_or("");
    let project_id = task["project_id"].as_str().unwrap_or("");
    let title = task["title"].as_str().unwrap_or("(task)");
    let priority = task["priority"].as_i64().unwrap_or(2);
    let phase = task["phase"].as_str();

    // Work is done — complete it first
    complete_task(cfg, client, task_id, output).await;

    // Pick reviewer: first online peer that is not me
    let reviewer = online_peers.iter()
        .find(|p| p.as_str() != cfg.agent_name.as_str())
        .map(|s| s.as_str())
        .unwrap_or("");

    let summary = &output[..output.len().min(2000)];
    let mut meta = serde_json::json!({"work_output_summary": summary});
    if !reviewer.is_empty() {
        meta["preferred_executor"] = Value::String(reviewer.to_string());
    }

    let review_desc = format!(
        "Review the completed work for task '{title}' (ID: {task_id}).\n\nWorker summary:\n{summary}\n\nCheck the shared project workspace for changes."
    );

    let mut body = serde_json::json!({
        "project_id": project_id,
        "title": format!("Review: {title}"),
        "description": review_desc,
        "task_type": "review",
        "review_of": task_id,
        "priority": priority,
        "metadata": meta,
    });
    if let Some(p) = phase {
        body["phase"] = Value::String(p.to_string());
    }

    let url = format!("{}/api/tasks", cfg.acc_url);
    match client.post(&url).bearer_auth(&cfg.acc_token).json(&body).send().await {
        Ok(resp) => {
            let review_id = resp.json::<Value>().await.ok()
                .and_then(|b| b["task"]["id"].as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            log(cfg, &format!("submitted {task_id} for review → {review_id} (reviewer: {})",
                if reviewer.is_empty() { "any" } else { reviewer }));
        }
        Err(e) => log(cfg, &format!("failed to create review task: {e}")),
    }
}

// ── Review task execution ─────────────────────────────────────────────────────

async fn execute_review_task(cfg: &Config, client: &reqwest::Client, task: &Value) {
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let review_of_id = task["review_of"].as_str().unwrap_or("");
    let phase = task["phase"].as_str().unwrap_or("");

    log(cfg, &format!("executing review {task_id} (reviewing {review_of_id})"));

    // Fetch original task to get project_id
    let project_id = fetch_task_project_id(cfg, client, review_of_id, task).await;

    let workspace = resolve_workspace(cfg, &project_id, "").await;
    let _ = std::fs::create_dir_all(&workspace);

    let work_summary = task["metadata"]["work_output_summary"].as_str().unwrap_or("");
    let ctx = serde_json::json!({
        "review_task": task,
        "review_of_id": review_of_id,
        "work_output_summary": work_summary,
    });
    let _ = std::fs::write(workspace.join(".review-context.json"), ctx.to_string());

    let review_output = run_review_subprocess(cfg, task, &workspace, review_of_id, work_summary).await;

    let (verdict, reason, gaps) = match review_output {
        Ok(out) => parse_review_output(&out),
        Err(e) => {
            log(cfg, &format!("review subprocess failed: {e}"));
            ("rejected".to_string(), format!("subprocess failed: {e}"), vec![])
        }
    };

    // File gap tasks
    for gap in &gaps {
        create_gap_task(cfg, client, &project_id, phase, task_id, gap).await;
    }

    // Record verdict on the original work task
    if !review_of_id.is_empty() {
        set_review_result_on_task(cfg, client, review_of_id, &verdict, &reason).await;
    }

    complete_task(cfg, client, task_id, &format!("verdict: {verdict}, reason: {reason}")).await;
    log(cfg, &format!("review {task_id} done: {verdict} ({} gaps filed)", gaps.len()));
}

async fn fetch_task_project_id(cfg: &Config, client: &reqwest::Client, task_id: &str, fallback_task: &Value) -> String {
    if task_id.is_empty() {
        return fallback_task["project_id"].as_str().unwrap_or("").to_string();
    }
    let url = format!("{}/api/tasks/{}", cfg.acc_url, task_id);
    match client.get(&url).bearer_auth(&cfg.acc_token).send().await {
        Ok(r) => r.json::<Value>().await.ok()
            .and_then(|b| b["project_id"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| fallback_task["project_id"].as_str().unwrap_or("").to_string()),
        Err(_) => fallback_task["project_id"].as_str().unwrap_or("").to_string(),
    }
}

async fn run_review_subprocess(
    _cfg: &Config,
    task: &Value,
    workspace: &PathBuf,
    review_of_id: &str,
    work_summary: &str,
) -> Result<String, String> {
    let title = task["title"].as_str().unwrap_or("(task)");
    let summary = &work_summary[..work_summary.len().min(2000)];

    let prompt = format!(
        "You are a code reviewer in an automated pair-programming workflow.\n\n\
         Original task: {title}\n\
         Original task ID: {review_of_id}\n\
         Worker's own summary: {summary}\n\n\
         The working directory contains the project files written by the worker.\n\n\
         Review this work and respond with ONLY a single valid JSON object — no prose, no markdown:\n\
         {{\n\
           \"verdict\": \"approved\",\n\
           \"reason\": \"<one sentence>\",\n\
           \"gaps\": [\n\
             {{\n\
               \"title\": \"<short task title for the gap>\",\n\
               \"description\": \"<what still needs to be done and why>\",\n\
               \"priority\": 1\n\
             }}\n\
           ]\n\
         }}\n\n\
         Replace \"approved\" with \"rejected\" if there is a serious defect that must be fixed \
         before this phase can be committed. Gaps may be filed even for approved work.\n\n\
         Check for: (1) task completion, (2) consistency with existing code style and architecture, \
         (3) any CI/CD blockers such as missing tests or broken imports, \
         (4) remaining gaps the original task left unaddressed."
    );

    let claude = which_claude();
    let mut cmd = Command::new(&claude);
    cmd.args(["-p", &prompt, "--dangerously-skip-permissions"])
       .current_dir(workspace)
       .kill_on_drop(true);

    let result = tokio::time::timeout(WORK_TIMEOUT, cmd.output()).await
        .map_err(|_| "review timed out".to_string())?
        .map_err(|e| format!("subprocess failed: {e}"))?;

    if result.status.success() {
        Ok(String::from_utf8_lossy(&result.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&result.stderr).to_string())
    }
}

fn parse_review_output(output: &str) -> (String, String, Vec<Value>) {
    let start = output.find('{').unwrap_or(output.len());
    let end = output.rfind('}').map(|i| i + 1).unwrap_or(output.len());
    if start >= end {
        return ("rejected".to_string(), "unparseable output".to_string(), vec![]);
    }
    match serde_json::from_str::<Value>(&output[start..end]) {
        Ok(v) => {
            let verdict = v["verdict"].as_str().unwrap_or("rejected").to_string();
            let reason = v["reason"].as_str().unwrap_or("").to_string();
            let gaps = v["gaps"].as_array().cloned().unwrap_or_default();
            (verdict, reason, gaps)
        }
        Err(_) => ("rejected".to_string(), "unparseable output".to_string(), vec![]),
    }
}

async fn create_gap_task(cfg: &Config, client: &reqwest::Client, project_id: &str, phase: &str, review_task_id: &str, gap: &Value) {
    let title = gap["title"].as_str().unwrap_or("Gap task").to_string();
    let description = gap["description"].as_str().unwrap_or("").to_string();
    let priority = gap["priority"].as_i64().unwrap_or(2);

    let mut body = serde_json::json!({
        "project_id": project_id,
        "task_type": "work",
        "title": title,
        "description": description,
        "priority": priority,
        "metadata": {"spawned_by_review": review_task_id},
    });
    if !phase.is_empty() {
        body["phase"] = Value::String(phase.to_string());
    }

    let url = format!("{}/api/tasks", cfg.acc_url);
    match client.post(&url).bearer_auth(&cfg.acc_token).json(&body).send().await {
        Ok(resp) => {
            let gap_id = resp.json::<Value>().await.ok()
                .and_then(|b| b["task"]["id"].as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            log(cfg, &format!("filed gap task {gap_id}: {title}"));
        }
        Err(e) => log(cfg, &format!("failed to create gap task: {e}")),
    }
}

async fn set_review_result_on_task(cfg: &Config, client: &reqwest::Client, task_id: &str, verdict: &str, reason: &str) {
    let url = format!("{}/api/tasks/{}/review-result", cfg.acc_url, task_id);
    let _ = client.put(&url)
        .bearer_auth(&cfg.acc_token)
        .json(&serde_json::json!({"agent": cfg.agent_name, "result": verdict, "notes": reason}))
        .send().await;
}

// ── Phase commit task execution ───────────────────────────────────────────────

async fn execute_phase_commit_task(cfg: &Config, client: &reqwest::Client, task: &Value) {
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let project_id = task["project_id"].as_str().unwrap_or("");
    let phase = task["phase"].as_str().unwrap_or("unknown");

    log(cfg, &format!("executing phase_commit {task_id}: phase={phase}"));

    let workspace = resolve_workspace(cfg, project_id, "").await;
    let branch = format!("phase/{phase}");
    let n_blocked = task["blocked_by"].as_array().map(|a| a.len()).unwrap_or(0);
    let commit_msg = format!("phase commit: {phase} ({n_blocked} tasks reviewed and approved)");

    match run_git_phase_commit(&workspace, &branch, &commit_msg).await {
        Ok(out) => {
            log(cfg, &format!("phase_commit {task_id}: pushed {branch}"));
            complete_task(cfg, client, task_id, &format!("pushed branch {branch}: {out}")).await;
        }
        Err(e) => {
            log(cfg, &format!("phase_commit {task_id} git failed: {e}"));
            unclaim_task(cfg, client, task_id).await;
            // File investigation task
            let url = format!("{}/api/tasks", cfg.acc_url);
            let _ = client.post(&url)
                .bearer_auth(&cfg.acc_token)
                .json(&serde_json::json!({
                    "project_id": project_id,
                    "task_type": "work",
                    "title": format!("Investigate git failure: phase {phase}"),
                    "description": format!("Phase commit failed for {task_id}: {e}"),
                    "priority": 0,
                }))
                .send().await;
        }
    }
}

async fn run_git_phase_commit(workspace: &PathBuf, branch: &str, commit_msg: &str) -> Result<String, String> {
    let ws = workspace.to_str().unwrap_or(".");

    let checkout = Command::new("git")
        .args(["-C", ws, "checkout", "-B", branch])
        .output().await
        .map_err(|e| format!("git checkout: {e}"))?;
    if !checkout.status.success() {
        return Err(String::from_utf8_lossy(&checkout.stderr).to_string());
    }

    let add = Command::new("git")
        .args(["-C", ws, "add", "-A"])
        .output().await
        .map_err(|e| format!("git add: {e}"))?;
    if !add.status.success() {
        return Err(String::from_utf8_lossy(&add.stderr).to_string());
    }

    let commit = Command::new("git")
        .args(["-C", ws, "commit", "-m", commit_msg])
        .output().await
        .map_err(|e| format!("git commit: {e}"))?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr).to_string();
        if !stderr.contains("nothing to commit") {
            return Err(stderr);
        }
    }

    let push = tokio::time::timeout(
        Duration::from_secs(600),
        Command::new("git").args(["-C", ws, "push", "origin", branch]).output()
    ).await
    .map_err(|_| "git push timed out".to_string())?
    .map_err(|e| format!("git push: {e}"))?;

    if push.status.success() {
        Ok(String::from_utf8_lossy(&push.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&push.stderr).to_string())
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

async fn resolve_workspace(cfg: &Config, project_id: &str, task_id: &str) -> PathBuf {
    let shared = cfg.acc_dir.join("shared");
    if !project_id.is_empty() && shared.exists() {
        let p = shared.join(project_id);
        if p.exists() { return p; }
    }
    if task_id.is_empty() {
        // Shared project workspace (for review and phase_commit tasks)
        cfg.acc_dir.join("shared").join(if project_id.is_empty() { "default" } else { project_id })
    } else {
        cfg.acc_dir.join("task-workspaces").join(task_id)
    }
}

async fn complete_task(cfg: &Config, client: &reqwest::Client, task_id: &str, output: &str) {
    let url = format!("{}/api/tasks/{}/complete", cfg.acc_url, task_id);
    let _ = client.put(&url)
        .bearer_auth(&cfg.acc_token)
        .json(&serde_json::json!({"agent": cfg.agent_name, "output": &output[..output.len().min(4096)]}))
        .send().await;
}

async fn unclaim_task(cfg: &Config, client: &reqwest::Client, task_id: &str) {
    let url = format!("{}/api/tasks/{}/unclaim", cfg.acc_url, task_id);
    let _ = client.put(&url)
        .bearer_auth(&cfg.acc_token)
        .json(&serde_json::json!({"agent": cfg.agent_name}))
        .send().await;
}

fn which_claude() -> String {
    for path in &["/usr/local/bin/claude", "/usr/bin/claude"] {
        if std::path::Path::new(path).exists() {
            return path.to_string();
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        for rel in &[".local/bin/claude", ".claude/local/claude"] {
            let p = format!("{home}/{rel}");
            if std::path::Path::new(&p).exists() { return p; }
        }
    }
    "claude".to_string()
}

fn is_quenched(cfg: &Config) -> bool {
    cfg.quench_file().exists()
}

fn log(cfg: &Config, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [tasks] [{}] {msg}\n", cfg.agent_name);
    eprint!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_mock::{HubMock, HubState};
    use serde_json::json;

    fn test_cfg(url: &str) -> Config {
        Config {
            acc_dir: std::path::PathBuf::from("/tmp"),
            acc_url: url.to_string(),
            acc_token: "test-token".to_string(),
            agent_name: "test-agent".to_string(),
            agentbus_token: String::new(),
            pair_programming: true,
            host: "test-host.local".to_string(),
            ssh_user: "testuser".into(),
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
        }
    }

    fn test_cfg_no_pp(url: &str) -> Config {
        Config { pair_programming: false, ..test_cfg(url) }
    }

    // ── fetch_open_tasks ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_open_tasks_parses_tasks() {
        let mock = HubMock::with_tasks(vec![
            json!({"id": "t-1", "title": "Alpha", "status": "open", "task_type": "work"}),
            json!({"id": "t-2", "title": "Beta",  "status": "open", "task_type": "work"}),
        ]).await;
        let client = reqwest::Client::new();
        let tasks = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "work").await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["id"], "t-1");
    }

    #[tokio::test]
    async fn test_fetch_open_tasks_empty_hub() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        let tasks = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "work").await.unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_open_tasks_only_open_status() {
        let mock = HubMock::with_tasks(vec![
            json!({"id": "open-1",   "status": "open",    "task_type": "work"}),
            json!({"id": "claimed-1","status": "claimed", "task_type": "work"}),
        ]).await;
        let client = reqwest::Client::new();
        let tasks = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "work").await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "open-1");
    }

    #[tokio::test]
    async fn test_fetch_open_tasks_filters_by_task_type() {
        let mock = HubMock::with_tasks(vec![
            json!({"id": "w-1", "status": "open", "task_type": "work"}),
            json!({"id": "r-1", "status": "open", "task_type": "review"}),
            json!({"id": "p-1", "status": "open", "task_type": "phase_commit"}),
        ]).await;
        let client = reqwest::Client::new();
        let work = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "work").await.unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0]["id"], "w-1");

        let review = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "review").await.unwrap();
        assert_eq!(review.len(), 1);
        assert_eq!(review[0]["id"], "r-1");
    }

    #[tokio::test]
    async fn test_fetch_open_tasks_hub_unreachable() {
        let cfg = test_cfg("http://127.0.0.1:1");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build().unwrap();
        let result = fetch_open_tasks(&cfg, &client, 5, "work").await;
        assert!(result.is_err(), "unreachable hub must return Err");
    }

    // ── count_active_tasks ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_count_active_tasks_returns_claimed_count() {
        let mock = HubMock::with_state(HubState {
            tasks: vec![
                json!({"id": "c1", "status": "claimed"}),
                json!({"id": "c2", "status": "claimed"}),
                json!({"id": "o1", "status": "open"}),
            ],
            ..Default::default()
        }).await;
        let client = reqwest::Client::new();
        let count = count_active_tasks(&test_cfg(&mock.url), &client).await;
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_count_active_tasks_zero_when_none_claimed() {
        let mock = HubMock::with_tasks(vec![
            json!({"id": "o1", "status": "open"}),
        ]).await;
        let client = reqwest::Client::new();
        let count = count_active_tasks(&test_cfg(&mock.url), &client).await;
        assert_eq!(count, 0);
    }

    // ── claim_task ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_claim_task_success_returns_task() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        let result = claim_task(&test_cfg(&mock.url), &client, "task-xyz").await;
        assert!(result.is_ok(), "200 → Ok");
        assert_eq!(result.unwrap()["id"], "task-xyz");
    }

    #[tokio::test]
    async fn test_claim_task_conflict_returns_err_409() {
        let mock = HubMock::with_state(HubState { task_claim_status: 409, ..Default::default() }).await;
        let client = reqwest::Client::new();
        let result = claim_task(&test_cfg(&mock.url), &client, "task-abc").await;
        assert!(matches!(result, Err(409)), "409 → Err(409)");
    }

    #[tokio::test]
    async fn test_claim_task_rate_limited_returns_err_429() {
        let mock = HubMock::with_state(HubState { task_claim_status: 429, ..Default::default() }).await;
        let client = reqwest::Client::new();
        let result = claim_task(&test_cfg(&mock.url), &client, "task-def").await;
        assert!(matches!(result, Err(429)), "429 → Err(429)");
    }

    #[tokio::test]
    async fn test_claim_task_blocked_returns_err_423() {
        let mock = HubMock::with_state(HubState { task_claim_status: 423, ..Default::default() }).await;
        let client = reqwest::Client::new();
        let result = claim_task(&test_cfg(&mock.url), &client, "task-blocked").await;
        assert!(matches!(result, Err(423)), "423 → Err(423)");
    }

    // ── parse_review_output ───────────────────────────────────────────────────

    #[test]
    fn test_parse_review_output_approved() {
        let output = r#"{"verdict":"approved","reason":"looks good","gaps":[]}"#;
        let (v, r, g) = parse_review_output(output);
        assert_eq!(v, "approved");
        assert_eq!(r, "looks good");
        assert!(g.is_empty());
    }

    #[test]
    fn test_parse_review_output_approved_with_preamble() {
        let output = r#"Here is my review:

{"verdict":"approved","reason":"well done","gaps":[{"title":"Add tests","description":"Missing unit tests","priority":2}]}"#;
        let (v, r, g) = parse_review_output(output);
        assert_eq!(v, "approved");
        assert_eq!(r, "well done");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0]["title"], "Add tests");
    }

    #[test]
    fn test_parse_review_output_rejected() {
        let output = r#"{"verdict":"rejected","reason":"build is broken","gaps":[{"title":"Fix CI","description":"pipeline fails","priority":0}]}"#;
        let (v, r, g) = parse_review_output(output);
        assert_eq!(v, "rejected");
        assert_eq!(r, "build is broken");
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn test_parse_review_output_unparseable_treated_as_rejected() {
        let output = "This is not JSON at all";
        let (v, r, _) = parse_review_output(output);
        assert_eq!(v, "rejected");
        assert_eq!(r, "unparseable output");
    }

    #[test]
    fn test_parse_review_output_empty_treated_as_rejected() {
        let (v, r, _) = parse_review_output("");
        assert_eq!(v, "rejected");
        assert_eq!(r, "unparseable output");
    }

    // ── submit_for_review ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_submit_for_review_picks_non_self_peer() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        let cfg = Config {
            agent_name: "boris".to_string(),
            pair_programming: true,
            ..test_cfg(&mock.url)
        };
        let task = json!({"id":"t-1","project_id":"proj","title":"Do work","priority":2});
        let peers = vec!["natasha".to_string(), "boris".to_string()];

        submit_for_review(&cfg, &client, &task, "output here", &peers).await;

        let created = mock.state.read().await.created_tasks.lock().await.clone();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["task_type"], "review");
        assert_eq!(created[0]["review_of"], "t-1");
        assert_eq!(created[0]["metadata"]["preferred_executor"], "natasha");
    }

    #[tokio::test]
    async fn test_submit_for_review_no_peers_no_preferred() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        let cfg = Config {
            agent_name: "natasha".to_string(),
            pair_programming: true,
            ..test_cfg(&mock.url)
        };
        let task = json!({"id":"t-2","project_id":"proj","title":"Solo work","priority":2});

        submit_for_review(&cfg, &client, &task, "done", &[]).await;

        let created = mock.state.read().await.created_tasks.lock().await.clone();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["task_type"], "review");
        // No preferred_executor when no peers
        assert!(created[0]["metadata"]["preferred_executor"].is_null() ||
                created[0]["metadata"].get("preferred_executor").is_none());
    }

    #[tokio::test]
    async fn test_submit_for_review_self_only_peer_no_preferred() {
        let mock = HubMock::new().await;
        let client = reqwest::Client::new();
        let cfg = Config {
            agent_name: "natasha".to_string(),
            pair_programming: true,
            ..test_cfg(&mock.url)
        };
        let task = json!({"id":"t-3","project_id":"proj","title":"Solo work","priority":2});
        let peers = vec!["natasha".to_string()]; // only self

        submit_for_review(&cfg, &client, &task, "done", &peers).await;

        let created = mock.state.read().await.created_tasks.lock().await.clone();
        assert_eq!(created.len(), 1);
        // No other peer available, preferred_executor should be absent or empty
        let pref = created[0]["metadata"]["preferred_executor"].as_str().unwrap_or("");
        assert!(pref.is_empty());
    }
}
