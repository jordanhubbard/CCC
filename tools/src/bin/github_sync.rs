use clap::Parser;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(name = "github-sync", about = "Two-way GitHub ↔ beads ↔ fleet task sync")]
struct Args {
    /// Run once and exit (default)
    #[arg(long)]
    once: bool,
    /// Run as a polling daemon
    #[arg(long)]
    daemon: bool,
    /// No writes (dry-run mode)
    #[arg(long)]
    dry_run: bool,
    /// owner/repo overrides (space-separated; falls back to GITHUB_REPOS env)
    #[arg(value_name = "REPO")]
    repos: Vec<String>,
}

fn dispatch_label() -> String {
    std::env::var("GITHUB_DISPATCH_LABEL").unwrap_or_else(|_| "agent-ready".to_string())
}

fn sync_interval() -> u64 {
    std::env::var("GITHUB_SYNC_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300)
}

fn data_dir() -> PathBuf {
    std::env::var("ACC_DATA_DIR")
        .or_else(|_| std::env::var("CCC_DATA_DIR"))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{home}/.acc/data")
        })
        .into()
}

fn state_path() -> PathBuf {
    data_dir().join("github-sync-state.json")
}

// ── State ─────────────────────────────────────────────────────────────────

fn load_state(path: &PathBuf) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}))
}

fn save_state(path: &PathBuf, state: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let s = serde_json::to_string_pretty(state).unwrap_or_default() + "\n";
    if std::fs::write(&tmp, s).is_ok() {
        std::fs::rename(tmp, path).ok();
    }
}

// ── GitHub helpers ────────────────────────────────────────────────────────

fn gh_issue_list(repo: &str) -> Vec<Value> {
    let out = Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            "all",
            "--limit",
            "200",
            "--json",
            "number,title,body,labels,state,url,author,createdAt,updatedAt",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut issues: Vec<Value> =
                serde_json::from_str(&text).unwrap_or_default();
            for issue in &mut issues {
                issue["repo"] = json!(repo);
            }
            issues
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            eprintln!("WARN gh issue list failed for {repo}: {}", err.trim());
            vec![]
        }
        Err(e) => {
            eprintln!("WARN gh CLI error for {repo}: {e}");
            vec![]
        }
    }
}

#[allow(dead_code)]
fn gh_issue_comment(repo: &str, number: i64, body: &str, dry_run: bool) -> bool {
    if dry_run {
        println!(
            "  [dry-run] would comment on {repo}#{number}: {}",
            &body[..body.len().min(60)]
        );
        return true;
    }
    Command::new("gh")
        .args([
            "issue",
            "comment",
            &number.to_string(),
            "--repo",
            repo,
            "--body",
            body,
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[allow(dead_code)]
fn gh_issue_close(repo: &str, number: i64, dry_run: bool) -> bool {
    if dry_run {
        println!("  [dry-run] would close {repo}#{number}");
        return true;
    }
    Command::new("gh")
        .args(["issue", "close", &number.to_string(), "--repo", repo])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── beads helpers ─────────────────────────────────────────────────────────

fn bd_bin() -> String {
    which::which("bd")
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            format!("{home}/.local/bin/bd")
        })
}

fn bd(args: &[&str], dry_run: bool) -> (i32, String) {
    if dry_run
        && args
            .first()
            .map(|a| matches!(*a, "create" | "update" | "close"))
            .unwrap_or(false)
    {
        println!("  [dry-run] bd {}", args.join(" "));
        return (0, String::new());
    }
    let out = Command::new(bd_bin())
        .args(args)
        .output();
    match out {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
        ),
        Err(_) => (-1, String::new()),
    }
}

fn list_beads_issues() -> Vec<Value> {
    let (_, out) = bd(&["export"], false);
    let mut issues = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                issues.push(v);
            }
        }
    }
    issues
}

// ── ACC fleet API helpers ─────────────────────────────────────────────────

async fn acc_get(client: &reqwest::Client, acc_url: &str, token: &str, path: &str) -> Option<Value> {
    let resp = client
        .get(format!("{acc_url}{path}"))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    resp.json::<Value>().await.ok()
}

async fn acc_post(
    client: &reqwest::Client,
    acc_url: &str,
    token: &str,
    path: &str,
    body: &Value,
    dry_run: bool,
) -> Option<Value> {
    if dry_run {
        println!(
            "  [dry-run] POST {path}: {}",
            body.to_string().chars().take(120).collect::<String>()
        );
        return Some(json!({"ok": true, "task": {"id": "dry-run-task"}}));
    }
    let resp = client
        .post(format!("{acc_url}{path}"))
        .header("Authorization", format!("Bearer {token}"))
        .json(body)
        .send()
        .await
        .ok()?;
    resp.json::<Value>().await.ok()
}

async fn find_project_for_repo(
    client: &reqwest::Client,
    acc_url: &str,
    token: &str,
    repo: &str,
) -> Option<String> {
    let resp = acc_get(client, acc_url, token, "/api/projects").await?;
    let projects: Vec<Value> = if resp.is_array() {
        serde_json::from_value(resp).unwrap_or_default()
    } else {
        resp.get("projects")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    };
    let repo_name = repo.split('/').next_back().unwrap_or(repo).to_lowercase();
    for p in &projects {
        if p.get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase())
            .as_deref()
            == Some(&repo_name)
        {
            return p.get("id").and_then(|v| v.as_str()).map(str::to_owned);
        }
    }
    for p in &projects {
        if p.get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase())
            .as_deref()
            == Some("acc")
        {
            return p.get("id").and_then(|v| v.as_str()).map(str::to_owned);
        }
    }
    projects
        .first()
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn append_fleet_task_to_notes(beads_id: &str, task_id: &str, dry_run: bool) {
    let (_, out) = bd(&["export"], false);
    for line in out.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let b: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if b.get("id").and_then(|v| v.as_str()) == Some(beads_id) {
            let existing = b.get("notes").and_then(|v| v.as_str()).unwrap_or("");
            let new_notes = format!("{existing} fleet_task_id={task_id}").trim().to_string();
            bd(
                &["update", beads_id, &format!("--notes={new_notes}")],
                dry_run,
            );
            return;
        }
    }
}

// ── Sync logic ────────────────────────────────────────────────────────────

fn gh_key(num: i64, repo: &str) -> String {
    format!("[gh:{repo}#{num}]")
}

fn parse_notes_meta(b: &Value) -> (Option<i64>, Option<String>) {
    // Check structured metadata
    if let Some(meta) = b.get("metadata").and_then(|v| v.as_object()) {
        let num = meta.get("github_number").and_then(|v| v.as_i64());
        let r = meta
            .get("github_repo")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        if num.is_some() && r.is_some() {
            return (num, r);
        }
    }
    // Fall back to notes key=value
    let notes = b.get("notes").and_then(|v| v.as_str()).unwrap_or("");
    let mut num = None;
    let mut repo = None;
    for token in notes.split_whitespace() {
        if let Some((k, v)) = token.split_once('=') {
            match k {
                "github_number" => num = v.parse::<i64>().ok(),
                "github_repo" => repo = Some(v.to_string()),
                _ => {}
            }
        }
    }
    // Fall back to [gh:repo#number] in title
    if num.is_none() || repo.is_none() {
        let title = b.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let re = regex_gh_key(title);
        if let Some((r, n)) = re {
            repo.get_or_insert(r);
            num.get_or_insert(n);
        }
    }
    (num, repo)
}

fn regex_gh_key(title: &str) -> Option<(String, i64)> {
    // Manual parse of [gh:repo#number]
    let start = title.find("[gh:")?;
    let rest = &title[start + 4..];
    let hash = rest.find('#')?;
    let end = rest.find(']')?;
    if hash >= end {
        return None;
    }
    let repo = rest[..hash].to_string();
    let num: i64 = rest[hash + 1..end].parse().ok()?;
    Some((repo, num))
}

#[allow(dead_code)]
fn is_already_synced(issue: &Value, existing: &[Value]) -> bool {
    let num = issue.get("number").and_then(|v| v.as_i64());
    let repo = issue.get("repo").and_then(|v| v.as_str());
    existing.iter().any(|b| {
        let (bn, br) = parse_notes_meta(b);
        bn == num && br.as_deref() == repo
    })
}

fn find_synced<'a>(issue: &Value, existing: &'a [Value]) -> Option<&'a Value> {
    let num = issue.get("number").and_then(|v| v.as_i64());
    let repo = issue.get("repo").and_then(|v| v.as_str());
    existing.iter().find(|b| {
        let (bn, br) = parse_notes_meta(b);
        bn == num && br.as_deref() == repo
    })
}

fn has_dispatch_label(issue: &Value, label: &str) -> bool {
    issue
        .get("labels")
        .and_then(|l| l.as_array())
        .map(|labels| {
            labels
                .iter()
                .any(|lb| lb.get("name").and_then(|v| v.as_str()) == Some(label))
        })
        .unwrap_or(false)
}

fn label_names(issue: &Value) -> Vec<String> {
    issue
        .get("labels")
        .and_then(|l| l.as_array())
        .map(|labels| {
            labels
                .iter()
                .filter_map(|lb| lb.get("name").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn map_priority(labels: &[String]) -> i64 {
    for name in labels {
        match name.as_str() {
            "P0" | "critical" => return 0,
            "P1" | "bug" | "high" => return 1,
            "P2" | "enhancement" | "medium" => return 2,
            "P3" | "low" => return 3,
            "P4" | "backlog" => return 4,
            _ => {}
        }
    }
    2
}

fn build_fleet_task_payload(issue: &Value, beads_id: &str, project_id: &str) -> Value {
    let labels = label_names(issue);
    let desc = issue
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let url = issue.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let number = issue.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
    let title = issue.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let repo = issue.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    let gh_ref = format!("\n\nGitHub: {url}  |  Beads: {beads_id}");
    json!({
        "title": format!("{title} (#{number}, {beads_id})"),
        "description": desc + &gh_ref,
        "project_id": project_id,
        "task_type": "work",
        "phase": "build",
        "priority": map_priority(&labels),
        "metadata": {
            "source": "github",
            "github_number": number,
            "github_repo": repo,
            "github_url": url,
            "beads_id": beads_id,
        },
    })
}

async fn sync_repo(
    repo: &str,
    state: &mut Value,
    existing_beads: &[Value],
    client: &reqwest::Client,
    acc_url: &str,
    token: &str,
    dry_run: bool,
) -> (i64, i64, i64) {
    let issues = gh_issue_list(repo);
    if issues.is_empty() {
        return (0, 0, 0);
    }

    let project_id = find_project_for_repo(client, acc_url, token, repo).await;
    let mut created = 0i64;
    let mut updated = 0i64;
    let mut fleet_created = 0i64;
    let mut newest_ts = state
        .get(repo)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let label = dispatch_label();

    for issue in &issues {
        let ts = issue
            .get("updatedAt")
            .or_else(|| issue.get("createdAt"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if ts > newest_ts.as_str() {
            newest_ts = ts.to_string();
        }

        let labels = label_names(issue);
        let priority = map_priority(&labels);
        let number = issue.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
        let existing = find_synced(issue, existing_beads);

        if issue
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            == "CLOSED"
        {
            if let Some(b) = existing {
                if b.get("status").and_then(|v| v.as_str()) == Some("open") {
                    let bid = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  closing beads {bid} (GH #{number} closed)");
                    bd(&["close", bid, "--reason=closed on GitHub"], dry_run);
                    updated += 1;
                }
            }
            continue;
        }

        let url = issue.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let title = issue
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let body = issue.get("body").and_then(|v| v.as_str()).unwrap_or("");

        if existing.is_none() {
            println!(
                "  creating beads issue for {repo}#{number}: {}",
                &title[..title.len().min(60)]
            );
            let gk = gh_key(number, repo);
            let full_title = format!("{title} {gk}");
            let notes = format!(
                "source=github github_number={number} github_repo={repo} github_url={url}"
            );
            let (rc, out) = bd(
                &[
                    "create",
                    &format!("--title={full_title}"),
                    &format!("--description={body}"),
                    "--type=feature",
                    &format!("--priority={priority}"),
                    &format!("--notes={notes}"),
                ],
                dry_run,
            );
            if rc == 0 {
                created += 1;
                // Extract beads ID from output like "Created issue: CCC-xyz — ..."
                let beads_id = extract_beads_id(&out);
                if let Some(bid) = beads_id {
                    if has_dispatch_label(issue, &label) {
                        if let Some(ref pid) = project_id {
                            let payload = build_fleet_task_payload(issue, &bid, pid);
                            if let Some(resp) =
                                acc_post(client, acc_url, token, "/api/tasks", &payload, dry_run)
                                    .await
                            {
                                if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                                    let task_id = resp
                                        .pointer("/task/id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    println!("    → fleet task {task_id}");
                                    fleet_created += 1;
                                    let new_notes = format!(
                                        "{notes} fleet_task_id={task_id}"
                                    );
                                    bd(
                                        &["update", &bid, &format!("--notes={new_notes}")],
                                        dry_run,
                                    );
                                }
                            }
                        }
                    } else {
                        bd(&["update", &bid, &format!("--notes={notes}")], dry_run);
                    }
                }
            }
        } else {
            let b = existing.unwrap();
            let existing_title = b.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let bid = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if existing_title != title {
                println!("  updating beads {bid}: title changed");
                bd(&["update", bid, &format!("--title={title}")], dry_run);
                updated += 1;
            }
            let existing_status = b.get("status").and_then(|v| v.as_str()).unwrap_or("open");
            let (_, _) = parse_notes_meta(b);
            let has_fleet = b
                .get("notes")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("fleet_task_id=");
            if !matches!(existing_status, "closed" | "cancelled")
                && has_dispatch_label(issue, &label)
                && !has_fleet
            {
                if let Some(ref pid) = project_id {
                    let payload = build_fleet_task_payload(issue, bid, pid);
                    if let Some(resp) =
                        acc_post(client, acc_url, token, "/api/tasks", &payload, dry_run).await
                    {
                        if resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                            let task_id = resp
                                .pointer("/task/id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            println!("    → fleet task {task_id} for existing beads {bid}");
                            fleet_created += 1;
                            append_fleet_task_to_notes(bid, task_id, dry_run);
                        }
                    }
                }
            }
        }
    }

    if !newest_ts.is_empty() {
        state[repo] = json!(newest_ts);
    }
    (created, updated, fleet_created)
}

fn extract_beads_id(out: &str) -> Option<String> {
    // Match any prefix like ACC-xyz, CCC-xyz, etc.
    for word in out.split_whitespace() {
        if word.len() >= 5
            && word.contains('-')
            && word[..word.find('-').unwrap()]
                .chars()
                .all(|c| c.is_ascii_uppercase())
        {
            let parts: Vec<&str> = word.splitn(2, '-').collect();
            if parts.len() == 2 && parts[0].len() >= 2 && parts[0].len() <= 6 {
                return Some(word.trim_end_matches(|c: char| !c.is_alphanumeric()).to_string());
            }
        }
    }
    None
}

// ── Main ──────────────────────────────────────────────────────────────────

async fn run_once(
    repos: &[String],
    dry_run: bool,
    client: &reqwest::Client,
    acc_url: &str,
    token: &str,
) -> Value {
    if repos.is_empty() {
        eprintln!("WARN: no repos configured — set GITHUB_REPOS=owner/repo,...");
        return json!({});
    }

    let sp = state_path();
    let mut state = load_state(&sp);
    let existing_beads = list_beads_issues();
    let mut results = json!({});

    for repo in repos {
        println!("Syncing {repo}…");
        let (c, u, f) = sync_repo(
            repo,
            &mut state,
            &existing_beads,
            client,
            acc_url,
            token,
            dry_run,
        )
        .await;
        results[repo] = json!({"created": c, "updated": u, "fleet_tasks": f});
        println!("  {repo}: +{c} created, ~{u} updated, {f} fleet tasks");
    }

    if !dry_run {
        save_state(&sp, &state);
        // Export beads JSONL for git backup
        if let Ok(exe) = std::env::current_exe() {
            let beads_dir = exe
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|p| p.join(".beads").join("issues.jsonl"));
            if let Some(path) = beads_dir {
                Command::new(bd_bin())
                    .args(["export", "--output", &path.to_string_lossy()])
                    .output()
                    .ok();
            }
        }
    }

    results
}

#[tokio::main]
async fn main() {
    acc_tools::load_acc_env();
    let mut args = Args::parse();

    if args.dry_run {
        unsafe { std::env::set_var("DRY_RUN", "true") };
    }

    let configured_repos: Vec<String> = std::env::var("GITHUB_REPOS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();

    if args.repos.is_empty() {
        args.repos = configured_repos;
    }

    let acc_url = acc_tools::acc_url();
    let token = acc_tools::acc_token();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("build reqwest client");

    if args.daemon {
        println!(
            "github-sync daemon starting — interval={}s repos={:?}",
            sync_interval(),
            args.repos
        );
        loop {
            run_once(&args.repos, args.dry_run, &client, &acc_url, &token).await;
            tokio::time::sleep(std::time::Duration::from_secs(sync_interval())).await;
        }
    } else {
        let result =
            run_once(&args.repos, args.dry_run, &client, &acc_url, &token).await;
        println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
    }
}
