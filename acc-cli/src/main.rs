//! acc — ACC fleet CLI
//!
//! Config (lowest to highest priority):
//!   ~/.acc/.env       ACC_HUB_URL, ACC_TOKEN / ACC_AGENT_TOKEN
//!   environment vars  ACC_HUB_URL, ACC_TOKEN
//!   --hub / --token flags
//!
//! Examples:
//!   acc tasks list --status=open
//!   acc tasks create --title "Fix bug" --project proj-abc
//!   acc tasks complete task-123 --output "done"
//!   acc agents list --online
//!   acc bus tail --filter github

use acc_client::{auth, Client};
use acc_model::{
    Agent, BusMsg, BusSendRequest, CreateProjectRequest, CreateTaskRequest, Project, QueueItem,
    Task, TaskStatus, TaskType,
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::str::FromStr;

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "acc",
    about = "ACC fleet CLI — tasks, projects, agents, bus",
    version
)]
struct Cli {
    /// Hub base URL
    #[arg(long, env = "ACC_HUB_URL", default_value = "http://localhost:8789")]
    hub: String,

    /// API bearer token (falls back to ACC_TOKEN env, then ~/.acc/.env)
    #[arg(long, env = "ACC_TOKEN")]
    token: Option<String>,

    /// Emit raw JSON instead of formatted output
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fleet task operations
    Tasks {
        #[command(subcommand)]
        sub: TaskCmd,
    },
    /// Project operations
    Projects {
        #[command(subcommand)]
        sub: ProjectCmd,
    },
    /// Agent inspection
    Agents {
        #[command(subcommand)]
        sub: AgentCmd,
    },
    /// Message bus
    Bus {
        #[command(subcommand)]
        sub: BusCmd,
    },
    /// Work queue
    Queue {
        #[command(subcommand)]
        sub: QueueCmd,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    /// List tasks
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, name = "type")]
        task_type: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value = "50")]
        limit: u32,
    },
    /// Show a task
    Get { id: String },
    /// Create a task
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "2")]
        priority: i64,
        #[arg(long, name = "type", default_value = "work")]
        task_type: String,
    },
    /// Mark a task complete
    Complete {
        id: String,
        #[arg(long)]
        output: Option<String>,
    },
    /// Cancel a task
    Cancel { id: String },
    /// Claim a task for an agent
    Claim {
        id: String,
        #[arg(long)]
        agent: String,
    },
    /// Release a claimed task
    Unclaim { id: String },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// List projects
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        q: Option<String>,
        #[arg(long, default_value = "50")]
        limit: u32,
    },
    /// Show a project
    Get { id: String },
    /// Create a project
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Archive or delete a project
    Delete {
        id: String,
        #[arg(long)]
        hard: bool,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    /// List agents
    List {
        #[arg(long)]
        online: bool,
    },
    /// Show an agent
    Get { name: String },
}

#[derive(Subcommand)]
enum BusCmd {
    /// Stream the bus live (tail -f style)
    Tail {
        /// Only show messages whose type contains this string
        #[arg(long)]
        filter: Option<String>,
    },
    /// Send a message to the bus
    Send {
        /// Message type
        #[arg(long, name = "type")]
        msg_type: String,
        /// Optional JSON body (merged with type)
        #[arg(long)]
        body: Option<String>,
    },
    /// Query recent messages
    Messages {
        #[arg(long)]
        msg_type: Option<String>,
        #[arg(long, default_value = "50")]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum QueueCmd {
    /// List queue items
    List,
    /// Show a queue item
    Get { id: String },
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> &str {
    &s[..s.len().min(max)]
}

fn print_task(t: &Task) {
    let id = truncate(&t.id, 16);
    let typ = format!("{:<12}", task_type_str(t.task_type));
    let stat = format!("{:<10}", task_status_str(t.status));
    let agent_part = t
        .claimed_by
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|a| format!(" [{a}]"))
        .unwrap_or_default();
    println!(
        "{id}  {typ}  p{}  {stat}  {}{agent_part}",
        t.priority, t.title
    );
}

fn print_task_detail(t: &Task) {
    println!("id:          {}", t.id);
    println!("title:       {}", t.title);
    println!("type:        {}", task_type_str(t.task_type));
    println!("status:      {}", task_status_str(t.status));
    println!("priority:    {}", t.priority);
    println!("project:     {}", t.project_id);
    if let Some(phase) = &t.phase {
        println!("phase:       {phase}");
    }
    if let Some(by) = &t.claimed_by {
        println!("claimed_by:  {by}");
        if let Some(exp) = &t.claim_expires_at {
            println!("expires:     {}", exp.to_rfc3339());
        }
    }
    if let Some(by) = &t.completed_by {
        let at = t.completed_at.map(|d| d.to_rfc3339()).unwrap_or_default();
        println!("completed:   {at} by {by}");
    }
    if let Some(r) = t.review_result {
        println!("review:      {}", review_result_str(r));
    }
    if !t.description.is_empty() {
        println!("\n{}", t.description);
    }
}

fn task_status_str(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Open => "open",
        TaskStatus::Claimed => "claimed",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Completed => "completed",
        TaskStatus::Cancelled => "cancelled",
    }
}
fn task_type_str(t: TaskType) -> &'static str {
    match t {
        TaskType::Work => "work",
        TaskType::Review => "review",
        TaskType::Idea => "idea",
        TaskType::Discovery => "discovery",
        TaskType::PhaseCommit => "phase_commit",
        TaskType::Feature => "feature",
        TaskType::Bug => "bug",
        TaskType::Epic => "epic",
        TaskType::Task => "task",
        TaskType::Unknown => "unknown",
    }
}
fn review_result_str(r: acc_model::ReviewResult) -> &'static str {
    match r {
        acc_model::ReviewResult::Approved => "approved",
        acc_model::ReviewResult::Rejected => "rejected",
    }
}

fn print_project(p: &Project) {
    let id = truncate(&p.id, 16);
    let stat = format!(
        "{:<10}",
        p.status
            .map(|s| match s {
                acc_model::ProjectStatus::Active => "active",
                acc_model::ProjectStatus::Archived => "archived",
            })
            .unwrap_or("")
    );
    let repo_part = p
        .repo_url
        .as_deref()
        .or_else(|| p.extra.get("repo").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(|r| format!("  ({r})"))
        .unwrap_or_default();
    println!("{id}  {stat}  {}{repo_part}", p.name);
}

fn print_agent(a: &Agent) {
    let name = format!("{:<16}", a.name);
    let node = a
        .extra
        .get("node")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| a.host.clone())
        .unwrap_or_default();
    let node = format!("{:<20}", node);
    let status = if a.online.unwrap_or(false) {
        "online "
    } else {
        "offline"
    };
    println!("{name}  {status}  {node}");
}

fn print_bus_message(m: &BusMsg) {
    let ts =
        m.ts.map(|t| t.to_rfc3339())
            .or_else(|| {
                m.extra
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_default();
    let ts = if ts.len() > 19 { &ts[..19] } else { &ts };
    let typ = format!("{:<30}", m.kind.as_deref().unwrap_or(""));
    // Brief hint: first couple of meaningful fields from the payload.
    let mut hint = String::new();
    let mut count = 0;
    if let Some(from) = &m.from {
        hint.push_str(&format!("from={from}  "));
        count += 1;
    }
    if let Some(body) = &m.body {
        let as_str = body
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| body.to_string());
        let b = if as_str.len() > 40 {
            format!("{}…", &as_str[..40])
        } else {
            as_str
        };
        hint.push_str(&format!("body={b}  "));
        count += 1;
    }
    for (k, v) in &m.extra {
        if count >= 2 {
            break;
        }
        if k == "ts" || k == "seq" || k == "id" || k == "created_at" {
            continue;
        }
        let val = v
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| v.to_string());
        let val = if val.len() > 40 {
            format!("{}…", &val[..40])
        } else {
            val
        };
        hint.push_str(&format!("{k}={val}  "));
        count += 1;
    }
    println!("{ts}  {typ}  {}", hint.trim_end());
}

fn print_queue_item(item: &QueueItem) {
    let id = truncate(&item.id, 20);
    let stat = format!("{:<12}", item.status.as_deref().unwrap_or(""));
    let title = item
        .title
        .as_deref()
        .or_else(|| item.extra.get("task").and_then(|v| v.as_str()))
        .unwrap_or("");
    println!("{id}  {stat}  {title}");
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let token = auth::resolve_token(cli.token).context("resolving API token")?;
    let client = Client::new(&cli.hub, &token).context("building HTTP client")?;
    let json_out = cli.json;

    match cli.command {
        Cmd::Tasks { sub } => run_tasks(&client, sub, json_out).await,
        Cmd::Projects { sub } => run_projects(&client, sub, json_out).await,
        Cmd::Agents { sub } => run_agents(&client, sub, json_out).await,
        Cmd::Bus { sub } => run_bus(&client, sub, json_out).await,
        Cmd::Queue { sub } => run_queue(&client, sub, json_out).await,
    }
}

// ── Tasks ─────────────────────────────────────────────────────────────────────

async fn run_tasks(c: &Client, sub: TaskCmd, json_out: bool) -> Result<()> {
    match sub {
        TaskCmd::List {
            status,
            project,
            task_type,
            agent,
            limit,
        } => {
            let mut b = c.tasks().list().limit(limit);
            if let Some(s) = status {
                b = b.status(
                    TaskStatus::from_str(&s).with_context(|| format!("invalid --status {s}"))?,
                );
            }
            if let Some(t) = task_type {
                b = b.task_type(
                    TaskType::from_str(&t).with_context(|| format!("invalid --type {t}"))?,
                );
            }
            if let Some(p) = project {
                b = b.project(p);
            }
            if let Some(a) = agent {
                b = b.agent(a);
            }
            let tasks = b.send().await?;
            if json_out {
                print_json(&json!({ "tasks": tasks, "count": tasks.len() }))?;
            } else if tasks.is_empty() {
                println!("(no tasks)");
            } else {
                for t in &tasks {
                    print_task(t);
                }
                println!("\n{} task(s)", tasks.len());
            }
        }
        TaskCmd::Get { id } => {
            let task = c.tasks().get(&id).await?;
            if json_out {
                print_json(&task)?;
            } else {
                print_task_detail(&task);
            }
        }
        TaskCmd::Create {
            title,
            description,
            project,
            priority,
            task_type,
        } => {
            let req = CreateTaskRequest {
                project_id: project.unwrap_or_default(),
                title,
                description,
                priority: Some(priority),
                task_type: Some(
                    TaskType::from_str(&task_type)
                        .with_context(|| format!("invalid --type {task_type}"))?,
                ),
                ..Default::default()
            };
            let task = c.tasks().create(&req).await?;
            if json_out {
                print_json(&task)?;
            } else {
                println!("created {}", task.id);
            }
        }
        TaskCmd::Complete { id, output } => {
            c.tasks().complete(&id, None, output.as_deref()).await?;
            if json_out {
                print_json(&json!({"ok": true, "id": id}))?;
            } else {
                println!("completed {id}");
            }
        }
        TaskCmd::Cancel { id } => {
            c.tasks().cancel(&id).await?;
            if json_out {
                print_json(&json!({"ok": true, "id": id}))?;
            } else {
                println!("cancelled {id}");
            }
        }
        TaskCmd::Claim { id, agent } => {
            let task = c.tasks().claim(&id, &agent).await?;
            if json_out {
                print_json(&task)?;
            } else {
                println!("claimed {id} by {agent}");
            }
        }
        TaskCmd::Unclaim { id } => {
            c.tasks().unclaim(&id, None).await?;
            if json_out {
                print_json(&json!({"ok": true, "id": id}))?;
            } else {
                println!("unclaimed {id}");
            }
        }
    }
    Ok(())
}

// ── Projects ──────────────────────────────────────────────────────────────────

async fn run_projects(c: &Client, sub: ProjectCmd, json_out: bool) -> Result<()> {
    match sub {
        ProjectCmd::List { status, q, limit } => {
            let mut b = c.projects().list().limit(limit);
            if let Some(s) = status {
                b = b.status(s);
            }
            if let Some(s) = q {
                b = b.query(s);
            }
            let projects = b.send().await?;
            if json_out {
                print_json(&json!({ "projects": projects, "count": projects.len() }))?;
            } else if projects.is_empty() {
                println!("(no projects)");
            } else {
                for p in &projects {
                    print_project(p);
                }
                println!("\n{} project(s)", projects.len());
            }
        }
        ProjectCmd::Get { id } => {
            let p = c.projects().get(&id).await?;
            if json_out {
                print_json(&p)?;
            } else {
                println!("id:      {}", p.id);
                println!("name:    {}", p.name);
                if let Some(s) = p.status {
                    println!(
                        "status:  {}",
                        match s {
                            acc_model::ProjectStatus::Active => "active",
                            acc_model::ProjectStatus::Archived => "archived",
                        }
                    );
                }
                if let Some(r) = p
                    .repo_url
                    .as_deref()
                    .or_else(|| p.extra.get("repo").and_then(|v| v.as_str()))
                {
                    println!("repo:    {r}");
                }
                if let Some(d) = &p.description {
                    if !d.is_empty() {
                        println!("\n{d}");
                    }
                }
            }
        }
        ProjectCmd::Create {
            name,
            description,
            repo,
        } => {
            let req = CreateProjectRequest {
                name,
                description,
                repo,
            };
            let p = c.projects().create(&req).await?;
            if json_out {
                print_json(&p)?;
            } else {
                println!("created {}", p.id);
            }
        }
        ProjectCmd::Delete { id, hard } => {
            c.projects().delete(&id, hard).await?;
            if json_out {
                print_json(&json!({"ok": true, "id": id}))?;
            } else {
                println!("deleted {id}");
            }
        }
    }
    Ok(())
}

// ── Agents ────────────────────────────────────────────────────────────────────

async fn run_agents(c: &Client, sub: AgentCmd, json_out: bool) -> Result<()> {
    match sub {
        AgentCmd::List { online } => {
            let mut b = c.agents().list();
            if online {
                b = b.online(true);
            }
            let agents = b.send().await?;
            if json_out {
                print_json(&json!({ "agents": agents, "count": agents.len() }))?;
            } else if agents.is_empty() {
                println!("(no agents)");
            } else {
                for a in &agents {
                    print_agent(a);
                }
                println!("\n{} agent(s)", agents.len());
            }
        }
        AgentCmd::Get { name } => {
            let a = c.agents().get(&name).await?;
            if json_out {
                print_json(&a)?;
            } else {
                println!("name:       {}", a.name);
                let status = if a.online.unwrap_or(false) {
                    "online"
                } else {
                    "offline"
                };
                println!("status:     {status}");
                let node = a
                    .extra
                    .get("node")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .or_else(|| a.host.clone())
                    .unwrap_or_default();
                println!("node:       {node}");
                if let Some(ls) = a.last_seen {
                    println!("last_seen:  {}", ls.to_rfc3339());
                }
                if let Some(caps) = a.capabilities.as_ref().and_then(|v| v.as_object()) {
                    let names: Vec<&str> = caps.keys().map(|k| k.as_str()).collect();
                    if !names.is_empty() {
                        println!("caps:       {}", names.join(", "));
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Bus ───────────────────────────────────────────────────────────────────────

async fn run_bus(c: &Client, sub: BusCmd, json_out: bool) -> Result<()> {
    match sub {
        BusCmd::Tail { filter } => {
            eprintln!("Connecting to {}/api/bus/stream …", c.base_url());
            let stream = c.bus().stream();
            tokio::pin!(stream);
            eprintln!("Connected. Streaming (Ctrl-C to stop)\n");
            while let Some(msg) = stream.next().await {
                let msg = msg?;
                let kind = msg.kind.as_deref().unwrap_or("");
                if let Some(f) = &filter {
                    if !kind.contains(f.as_str()) {
                        continue;
                    }
                }
                if json_out {
                    println!("{}", serde_json::to_string(&msg)?);
                } else {
                    print_bus_message(&msg);
                }
            }
        }
        BusCmd::Send { msg_type, body } => {
            let extra = if let Some(b) = body {
                serde_json::from_str::<serde_json::Map<String, Value>>(&b)
                    .context("--body must be a JSON object")?
                    .into_iter()
                    .collect()
            } else {
                Default::default()
            };
            let req = BusSendRequest {
                kind: msg_type,
                extra,
                ..Default::default()
            };
            c.bus().send(&req).await?;
            if json_out {
                print_json(&json!({"ok": true}))?;
            } else {
                println!("sent");
            }
        }
        BusCmd::Messages { msg_type, limit } => {
            let msgs = c.bus().messages(Some(limit), msg_type.as_deref()).await?;
            if json_out {
                print_json(&json!({ "messages": msgs, "count": msgs.len() }))?;
            } else if msgs.is_empty() {
                println!("(no messages)");
            } else {
                for m in &msgs {
                    print_bus_message(m);
                }
                println!("\n{} message(s)", msgs.len());
            }
        }
    }
    Ok(())
}

// ── Queue ─────────────────────────────────────────────────────────────────────

async fn run_queue(c: &Client, sub: QueueCmd, json_out: bool) -> Result<()> {
    match sub {
        QueueCmd::List => {
            let items = c.queue().list().await?;
            if json_out {
                print_json(&json!({ "items": items, "count": items.len() }))?;
            } else if items.is_empty() {
                println!("(empty queue)");
            } else {
                for item in &items {
                    print_queue_item(item);
                }
                println!("\n{} item(s)", items.len());
            }
        }
        QueueCmd::Get { id } => {
            let item = c.queue().get(&id).await?;
            if json_out {
                print_json(&item)?;
            } else {
                println!("id:      {}", item.id);
                println!("status:  {}", item.status.as_deref().unwrap_or(""));
                let title = item
                    .title
                    .as_deref()
                    .or_else(|| item.extra.get("task").and_then(|v| v.as_str()))
                    .unwrap_or("");
                println!("title:   {title}");
                if let Some(c) = item.created {
                    println!("created: {}", c.to_rfc3339());
                }
            }
        }
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}
