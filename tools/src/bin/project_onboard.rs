use clap::Parser;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(name = "project-onboard", about = "Bootstrap a new project into the ACC fleet")]
struct Args {
    /// GitHub repo (owner/repo or full URL)
    #[arg(long)]
    repo: String,
    /// Human-readable project name
    #[arg(long, default_value = "")]
    name: String,
    /// Branch to clone
    #[arg(long, default_value = "main")]
    branch: String,
    /// Use an existing local clone instead of cloning
    #[arg(long, default_value = "")]
    local: String,
    /// Project description
    #[arg(long, default_value = "")]
    description: String,
}

fn log(msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    println!("[{ts}] [project-onboard] {msg}");
}

fn acc_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home".to_string());
    let acc = PathBuf::from(&home).join(".acc");
    if acc.exists() {
        acc
    } else {
        PathBuf::from(&home).join(".ccc")
    }
}

fn accfs_shared() -> PathBuf {
    let default = acc_dir().join("shared");
    std::env::var("ACC_SHARED_DIR")
        .map(PathBuf::from)
        .unwrap_or(default)
}

fn agent_name() -> String {
    std::env::var("AGENT_NAME").unwrap_or_else(|_| "unknown".to_string())
}

fn slug(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut result = String::new();
    let mut last_dash = true;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            result.push(ch);
            last_dash = false;
        } else if !last_dash {
            result.push('-');
            last_dash = true;
        }
    }
    result.trim_end_matches('-').to_string()
}

fn proj_id(repo: &str, branch: &str) -> String {
    use sha1::{Digest, Sha1};
    let input = format!("{repo}@{branch}");
    let hash = Sha1::digest(input.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("proj-{}", &hex[..8])
}

fn accfs_project_path(slug: &str) -> PathBuf {
    accfs_shared().join("projects").join(slug)
}

fn project_exists(slug: &str) -> bool {
    accfs_project_path(slug).join("project.json").exists()
}

fn clone_repo(repo: &str, branch: &str, dest: &Path) -> Result<String, String> {
    let url = if repo.contains("://") {
        repo.to_string()
    } else {
        format!("https://github.com/{repo}.git")
    };
    log(&format!("Cloning {url} @ {branch} → {}", dest.display()));

    let status = Command::new("git")
        .args([
            "clone",
            "--recurse-submodules",
            "--depth=50",
            "--branch",
            branch,
            &url,
            &dest.to_string_lossy(),
        ])
        .status();

    let ok = match status {
        Ok(s) if s.success() => true,
        _ => {
            // retry without --branch
            Command::new("git")
                .args([
                    "clone",
                    "--recurse-submodules",
                    "--depth=50",
                    &url,
                    &dest.to_string_lossy(),
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    };

    if !ok {
        return Err(format!("git clone failed for {repo}"));
    }

    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(dest)
        .output()
        .map_err(|e| e.to_string())?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    log(&format!("Cloned at {sha}"));
    Ok(sha)
}

fn rsync(src: &Path, dst: &Path) -> bool {
    std::fs::create_dir_all(dst).ok();
    let src_str = format!("{}/", src.display());
    let dst_str = format!("{}/", dst.display());
    let status = Command::new("rsync")
        .args(["-a", "--delete", "--quiet", &src_str, &dst_str])
        .status();
    match status {
        Ok(s) if s.success() => true,
        _ => {
            log("rsync failed");
            false
        }
    }
}

fn parse_plan(plan_path: &Path) -> Vec<String> {
    let mut tasks = Vec::new();
    let text = match std::fs::read_to_string(plan_path) {
        Ok(t) => t,
        Err(_) => return tasks,
    };
    let mut section = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            section = rest.trim().to_string();
            continue;
        }
        // checkbox items
        if let Some(m) = line
            .trim_start_matches(|c: char| c == ' ' || c == '\t')
            .strip_prefix("- [ ] ")
            .or_else(|| {
                line.trim_start_matches(|c: char| c == ' ' || c == '\t')
                    .strip_prefix("* [ ] ")
            })
            .or_else(|| {
                line.trim_start_matches(|c: char| c == ' ' || c == '\t')
                    .strip_prefix("- [x] ")
            })
            .or_else(|| {
                line.trim_start_matches(|c: char| c == ' ' || c == '\t')
                    .strip_prefix("- [X] ")
            })
            .or_else(|| {
                line.trim_start_matches(|c: char| c == ' ' || c == '\t')
                    .strip_prefix("* [x] ")
            })
        {
            let text = m.trim();
            tasks.push(if section.is_empty() {
                text.to_string()
            } else {
                format!("{section}: {text}")
            });
            continue;
        }
        // numbered list items in a section
        let trimmed = line.trim_start_matches(|c: char| c == ' ' || c == '\t');
        if !section.is_empty() {
            if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
                if rest.starts_with(". ") {
                    let text = rest[2..].trim();
                    tasks.push(format!("{section}: {text}"));
                }
            }
        }
    }
    log(&format!("Parsed {} task(s) from PLAN.md", tasks.len()));
    tasks
}

fn parse_beads(repo_dir: &Path) -> Vec<Value> {
    let issues_file = repo_dir.join(".beads").join("issues.jsonl");
    let text = match std::fs::read_to_string(&issues_file) {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let mut beads = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(issue) = serde_json::from_str::<Value>(line) {
            if issue.get("status").and_then(|s| s.as_str()) == Some("open") {
                beads.push(issue);
            }
        }
    }
    log(&format!("Found {} open bead(s)", beads.len()));
    beads
}

fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("build reqwest client")
}

async fn acc_post(
    client: &reqwest::Client,
    acc_url: &str,
    token: &str,
    path: &str,
    body: &Value,
) -> Option<Value> {
    if acc_url.is_empty() || token.is_empty() {
        return None;
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

async fn post_task(
    client: &reqwest::Client,
    acc_url: &str,
    token: &str,
    title: &str,
    description: &str,
    project_id: &str,
    accfs_path: &str,
    github_repo: &str,
    tags: &[&str],
    depends_on: &[String],
    bead_id: &str,
) -> Option<String> {
    let mut tag_list: Vec<&str> = tags.to_vec();
    tag_list.push("project");
    let mut item = json!({
        "title": &title[..title.len().min(120)],
        "description": description,
        "status": "pending",
        "priority": "normal",
        "assignee": "all",
        "source": agent_name(),
        "created": now_utc(),
        "attempts": 0,
        "maxAttempts": 3,
        "tags": tag_list,
        "project_id": project_id,
        "project_accfs_path": accfs_path,
        "project": github_repo,
    });
    if !bead_id.is_empty() {
        item["scout_key"] = json!(format!("bead:{bead_id}"));
    }
    if !depends_on.is_empty() {
        item["dependsOn"] = json!(depends_on);
    }
    let resp = acc_post(client, acc_url, token, "/api/queue", &item).await?;
    let id = resp
        .get("id")
        .or_else(|| resp.pointer("/item/id"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    if id.is_none() {
        let short = &title[..title.len().min(60)];
        log(&format!("WARNING: failed to post task '{short}'"));
    }
    id
}

#[tokio::main]
async fn main() {
    acc_tools::load_acc_env();
    let args = Args::parse();

    let name = if args.name.is_empty() {
        args.repo
            .split('/')
            .next_back()
            .unwrap_or(&args.repo)
            .to_string()
    } else {
        args.name.clone()
    };

    if let Err(e) = onboard(
        &args.repo,
        &name,
        &args.branch,
        if args.local.is_empty() {
            None
        } else {
            Some(PathBuf::from(&args.local))
        },
        &args.description,
    )
    .await
    {
        log(&format!("ERROR: {e}"));
        std::process::exit(1);
    }
}

async fn onboard(
    repo: &str,
    name: &str,
    branch: &str,
    local_path: Option<PathBuf>,
    description: &str,
) -> Result<(), String> {
    let slug = slug(name);
    let project_id = proj_id(repo, branch);
    let accfs_dir = accfs_project_path(&slug);
    let accfs_ws = accfs_dir.join("workspace");
    let accfs_ws_str = accfs_ws.to_string_lossy().to_string();

    log(&format!("Onboarding project: {name:?} ({project_id})"));
    log(&format!("  repo:   {repo} @ {branch}"));
    log(&format!("  accfs:  {accfs_ws_str}"));

    if project_exists(&slug) {
        log(&format!(
            "Project '{slug}' already exists in AccFS — re-running task generation only"
        ));
    }

    let acc_url = acc_tools::acc_url();
    let token = acc_tools::acc_token();
    let client = http_client();

    let tmp_dir = tempfile::Builder::new()
        .prefix("ccc-onboard-")
        .tempdir()
        .map_err(|e| e.to_string())?;

    let (repo_dir, sha) = match local_path {
        Some(p) => (p, "local".to_string()),
        None => {
            let dest = tmp_dir.path().join(&slug);
            let sha = clone_repo(repo, branch, &dest)?;
            (dest, sha)
        }
    };

    let shared = accfs_shared();
    if shared.exists() {
        log(&format!("Syncing to AccFS: {accfs_ws_str}"));
        rsync(&repo_dir, &accfs_ws);
    } else {
        log(&format!(
            "WARNING: AccFS not mounted at {} — skipping shared sync",
            shared.display()
        ));
    }

    let plan_tasks = parse_plan(&repo_dir.join("PLAN.md"));
    let bead_items = parse_beads(&repo_dir);

    let now = now_utc();
    let mut task_ids: Vec<String> = Vec::new();

    for title in &plan_tasks {
        if let Some(tid) = post_task(
            &client,
            &acc_url,
            &token,
            title,
            &format!("From PLAN.md in {repo}"),
            &project_id,
            &accfs_ws_str,
            repo,
            &["plan"],
            &[],
            "",
        )
        .await
        {
            let short = &title[..title.len().min(60)];
            log(&format!("  task: {tid} — {short}"));
            task_ids.push(tid);
        }
    }

    for bead in &bead_items {
        let bead_title = bead
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled bead");
        let bead_id_str = bead
            .get("id")
            .map(|v| v.to_string().trim_matches('"').to_string())
            .unwrap_or_default();
        let bead_desc = bead
            .get("description")
            .or_else(|| bead.get("body"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let bead_desc = if bead_desc.is_empty() {
            format!("Bead {bead_id_str} from {repo}: {bead_title}")
        } else {
            bead_desc
        };

        if let Some(tid) = post_task(
            &client,
            &acc_url,
            &token,
            bead_title,
            &bead_desc,
            &project_id,
            &accfs_ws_str,
            repo,
            &["beads"],
            &[],
            &bead_id_str,
        )
        .await
        {
            let short = &bead_title[..bead_title.len().min(60)];
            log(&format!("  task: {tid} — (bead) {short}"));
            task_ids.push(tid);
        }
    }

    let milestone_id: Option<String> = if !task_ids.is_empty() {
        let mid = post_task(
            &client,
            &acc_url,
            &token,
            &format!("[{name}] milestone: reconcile AccFS → GitHub"),
            &format!(
                "Sync completed project work from AccFS ({accfs_ws_str}) back to GitHub ({repo} @ {branch}).\n\n\
                1. Review all changes in the AccFS workspace\n\
                2. Run tests / build\n\
                3. Commit and push to a release branch\n\
                4. Open a PR or tag a release as appropriate"
            ),
            &project_id,
            &accfs_ws_str,
            repo,
            &["milestone", "sync"],
            &task_ids,
            "",
        )
        .await;
        if let Some(ref id) = mid {
            log(&format!(
                "  milestone: {id} (blocked on {} task(s))",
                task_ids.len()
            ));
        }
        mid
    } else {
        None
    };

    let project = json!({
        "id": project_id,
        "name": name,
        "slug": slug,
        "description": description,
        "status": "active",
        "github_repo": repo,
        "github_branch": branch,
        "github_sha": sha,
        "accfs_path": accfs_ws_str,
        "created_at": now,
        "created_by": agent_name(),
        "task_ids": task_ids,
        "milestone_task_id": milestone_id,
        "tags": [],
    });

    if shared.exists() {
        std::fs::create_dir_all(&accfs_dir).map_err(|e| e.to_string())?;
        let json_str = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
        std::fs::write(accfs_dir.join("project.json"), json_str)
            .map_err(|e| e.to_string())?;
        log(&format!("Project record saved: {}/project.json", accfs_dir.display()));
    }

    // Broadcast project.arrived
    let task_count = project["task_ids"].as_array().map(|a| a.len()).unwrap_or(0);
    let broadcast = json!({
        "from": agent_name(),
        "to": "all",
        "type": "project.arrived",
        "subject": "work",
        "body": serde_json::to_string(&json!({
            "project_id": project_id,
            "name": name,
            "slug": slug,
            "accfs_path": accfs_ws_str,
            "github_repo": repo,
            "task_count": task_count,
            "milestone_id": project["milestone_task_id"],
        })).unwrap_or_default(),
    });
    acc_post(&client, &acc_url, &token, "/bus/send", &broadcast).await;
    log(&format!(
        "Broadcast project.arrived → all agents ({task_count} task(s) available)"
    ));
    log(&format!("Done. Project {project_id} is live."));
    Ok(())
}
