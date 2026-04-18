//! Fleet task worker — polls /api/tasks, claims atomically, executes in AgentFS workspace.
//!
//! Replaces per-repo beads polling. The ACC server is the single source of truth.
//! Multiple agents run this concurrently; the server's SQL atomic claim prevents double-work.

use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use serde_json::Value;
use crate::config::Config;

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
    log(&cfg, &format!("starting (agent={}, hub={}, max_concurrent={})", cfg.agent_name, cfg.acc_url, max_concurrent));

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

        // How many tasks are we currently running?
        let active = count_active_tasks(&cfg, &client).await;
        if active >= max_concurrent {
            log(&cfg, &format!("at capacity ({}/{}), waiting", active, max_concurrent));
            sleep(POLL_IDLE).await;
            continue;
        }

        // Fetch open tasks
        let open_tasks = match fetch_open_tasks(&cfg, &client, max_concurrent - active).await {
            Ok(t) => t,
            Err(e) => {
                log(&cfg, &format!("fetch failed: {e}"));
                sleep(POLL_IDLE).await;
                continue;
            }
        };

        if open_tasks.is_empty() {
            sleep(POLL_IDLE).await;
            continue;
        }

        // Try to claim each task (first claim wins, others get 409)
        let mut claimed = false;
        for task in &open_tasks {
            let task_id = task["id"].as_str().unwrap_or("").to_string();
            if task_id.is_empty() { continue; }

            match claim_task(&cfg, &client, &task_id).await {
                Ok(claimed_task) => {
                    log(&cfg, &format!("claimed task {task_id}: {}", claimed_task["title"].as_str().unwrap_or("")));
                    let cfg2 = cfg.clone();
                    let client2 = client.clone();
                    let task2 = claimed_task.clone();
                    tokio::spawn(async move {
                        execute_task(&cfg2, &client2, &task2).await;
                    });
                    claimed = true;
                    break; // claim one per loop iteration, re-poll for more
                }
                Err(409) => { /* already claimed by another agent, try next */ }
                Err(429) => {
                    log(&cfg, "at capacity (server side)");
                    break;
                }
                Err(e) => {
                    log(&cfg, &format!("claim error {e} for {task_id}"));
                }
            }
        }

        sleep(if claimed { POLL_BUSY } else { POLL_IDLE }).await;
    }
}

async fn fetch_open_tasks(cfg: &Config, client: &reqwest::Client, limit: usize) -> Result<Vec<Value>, String> {
    let url = format!("{}/api/tasks?status=open&limit={}", cfg.acc_url, limit.max(1));
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

async fn execute_task(cfg: &Config, client: &reqwest::Client, task: &Value) {
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let title = task["title"].as_str().unwrap_or("(no title)");
    let project_id = task["project_id"].as_str().unwrap_or("");

    log(cfg, &format!("executing task {task_id}: {title}"));

    // Resolve workspace: prefer AgentFS project path, fall back to local tmp
    let workspace = resolve_workspace(cfg, project_id, task_id).await;
    let _ = std::fs::create_dir_all(&workspace);

    // Write task context file for the executing agent/claude process
    let ctx_path = workspace.join(".task-context.json");
    let _ = std::fs::write(&ctx_path, task.to_string());

    // Execute: run `claude -p <task description>` in the workspace if available
    let result = run_task_subprocess(cfg, task, &workspace).await;

    match result {
        Ok(output) => {
            log(cfg, &format!("task {task_id} completed: {}", &output[..output.len().min(120)]));
            complete_task(cfg, client, task_id, &output).await;
        }
        Err(e) => {
            log(cfg, &format!("task {task_id} failed: {e}"));
            unclaim_task(cfg, client, task_id).await;
        }
    }
}

async fn resolve_workspace(cfg: &Config, project_id: &str, task_id: &str) -> PathBuf {
    // Try AgentFS shared path first
    let shared = cfg.acc_dir.join("shared");
    if !project_id.is_empty() && shared.exists() {
        let p = shared.join(project_id);
        if p.exists() { return p; }
    }
    // Fall back to local task workspace
    cfg.acc_dir.join("task-workspaces").join(task_id)
}

async fn run_task_subprocess(cfg: &Config, task: &Value, workspace: &PathBuf) -> Result<String, String> {
    let description = task["description"].as_str().unwrap_or("");
    let title = task["title"].as_str().unwrap_or("(task)");

    let prompt = if description.is_empty() {
        title.to_string()
    } else {
        format!("{}\n\n{}", title, description)
    };

    // Find claude executable
    let claude = which_claude();

    let mut cmd = Command::new(&claude);
    cmd.arg("-p").arg(&prompt)
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
    // Check $HOME/.claude/local/claude
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{}/.claude/local/claude", home);
        if std::path::Path::new(&p).exists() { return p; }
    }
    "claude".to_string() // fallback: hope it's in PATH
}

fn is_quenched(cfg: &Config) -> bool {
    cfg.quench_file().exists()
}

fn log(cfg: &Config, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [tasks] [{}] {msg}\n", cfg.agent_name);
    eprint!("{line}");
    let log_path = cfg.log_file("tasks");
    let _ = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open(&log_path)
        .and_then(|mut f| { use std::io::Write; f.write_all(line.as_bytes()) });
}
