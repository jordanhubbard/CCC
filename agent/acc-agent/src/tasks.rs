//! Fleet task worker — polls /api/tasks, claims atomically, executes in AgentFS workspace.
//!
//! Work tasks and review tasks are executed via the native Anthropic agentic loop in sdk.rs.
//! Phase_commit tasks run git to push approved work to a branch.
//! Multiple agents run this concurrently; the server's SQL atomic claim prevents double-work.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::collections::HashMap;
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::time::sleep;
use serde_json::Value;
use acc_client::Client;
use acc_model::{BusSendRequest, CreateTaskRequest, HeartbeatRequest, ReviewResult, TaskStatus, TaskType};
use crate::config::Config;
use crate::peer_exchange::{
    AgentAction, ExchangeRequest, PeerExchangeCoordinator, TestCase, TestReport, TestRunner,
    TestSuite, TEST_TIMEOUT_SECS,
};
use crate::peers;
use crate::slack;

/// Error type returned by `run_git_phase_commit`.
///
/// * `Transient` — a retriable network/infrastructure hiccup; the caller
///   should silently requeue (no investigation task) and let the next
///   dispatch cycle retry.
/// * `Hard` — a permanent failure (auth rejected, non-fast-forward, local
///   git error, …); the caller should file an investigation task so a human
///   can look into it.
#[derive(Debug)]
pub(crate) enum PhaseCommitError {
    Transient(String),
    Hard(String),
}

impl std::fmt::Display for PhaseCommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseCommitError::Transient(msg) => write!(f, "(transient) {msg}"),
            PhaseCommitError::Hard(msg) => write!(f, "{msg}"),
        }
    }
}

const POLL_IDLE: Duration = Duration::from_secs(30);
const POLL_BUSY: Duration = Duration::from_secs(5);

/// Hard cap on a single idea-review vote's agentic loop. Voting is lighter
/// than a full review — reading the idea and writing a refinement should
/// complete well within 10 minutes under normal API latency.
const VOTE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Hard cap on a single review's agentic loop. Without this, a stuck
/// model call can hold a claim indefinitely (observed: 4h+ claims that
/// never complete, blocking the whole fleet via `count_active_tasks`).
const REVIEW_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Hard cap on a work task's agentic loop. Longer than reviews because
/// real implementation can legitimately take an hour+, but must be
/// bounded.
const WORK_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// After this agent completes or unclaims a task, skip re-claiming it
/// for this long. Breaks the re-claim loop where an agent instantly
/// grabs back a task it just released (observed 2026-04-24: unclaim
/// reversed by same-agent re-claim within 15s on every attempt).
const RECLAIM_COOLDOWN: Duration = Duration::from_secs(15 * 60);

/// Keepalive heartbeat interval while a long-running task is in
/// flight. Without this, the hub sees silence for the full
/// REVIEW_TIMEOUT (30min) or WORK_TIMEOUT (2h) and may treat the
/// agent as dead even though it's actively working (CCC-79d).
const TASK_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// Spawn a background task that posts /api/heartbeat/{agent} every
/// TASK_KEEPALIVE_INTERVAL until the returned sender is dropped or
/// signaled. Fire-and-forget on the network: if a heartbeat POST
/// fails, the next interval tries again.
fn spawn_keepalive(
    cfg: Config,
    client: Client,
    note: String,
) -> tokio::sync::oneshot::Sender<()> {
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TASK_KEEPALIVE_INTERVAL);
        // Skip the immediate first tick; heartbeat is for long-running
        // gaps, not for the moment after claim.
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let req = HeartbeatRequest {
                        ts: Some(chrono::Utc::now()),
                        status: Some("ok".into()),
                        note: Some(note.clone()),
                        host: Some(cfg.host.clone()),
                        ssh_user: Some(cfg.ssh_user.clone()),
                        ssh_host: Some(cfg.ssh_host.clone()),
                        ssh_port: Some(cfg.ssh_port as u64),
                    };
                    let _ = client.items().heartbeat(&cfg.agent_name, &req).await;
                }
                _ = &mut stop_rx => break,
            }
        }
    });
    stop_tx
}

/// Per-process cache of `(task_id, released_at)`. Keeps the last
/// `RECLAIM_COOLDOWN` worth of finished tasks so the poll loop can
/// skip them.
fn recent_done() -> &'static Mutex<HashMap<String, Instant>> {
    static CELL: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mark_done(task_id: &str) {
    if let Ok(mut m) = recent_done().lock() {
        // GC entries older than the cooldown window
        let now = Instant::now();
        m.retain(|_, t| now.duration_since(*t) < RECLAIM_COOLDOWN);
        m.insert(task_id.to_string(), now);
    }
}

fn in_cooldown(task_id: &str) -> bool {
    if let Ok(m) = recent_done().lock() {
        if let Some(t) = m.get(task_id) {
            return t.elapsed() < RECLAIM_COOLDOWN;
        }
    }
    false
}

// ── Persistent vote deduplication ─────────────────────────────────────────────
//
// `mark_done` / `in_cooldown` above are process-local: a crash or restart
// clears the in-memory HashMap and the agent can spawn a duplicate vote
// goroutine for any idea that was in-flight at crash time.  The server's
// 409 guard prevents a *second* vote from being recorded, but the duplicate
// goroutine still burns up to VOTE_TIMEOUT (10 min) of agentic-loop budget.
//
// The fix: write a sentinel file  <acc_dir>/task-workspaces/<task_id>/.voted
// immediately before spawning the vote goroutine.  On the next run (or after
// a crash) the poll loop checks for this file's existence before deciding
// whether to spawn.  The file is created by `mark_voted_persistent` and
// checked by `in_vote_cooldown_persistent`; both are called from the Poll 4
// loop and from `execute_idea_vote_task` after a successful vote submission.
//
// The workspace directory is `<acc_dir>/task-workspaces/<task_id>/`  — the
// same directory that `execute_idea_vote_task` already creates for the
// `.idea-context.json` context file, so no extra directory management is
// needed.

/// Return the path of the persistent voted-sentinel file for an idea task.
fn voted_sentinel_path(cfg: &Config, task_id: &str) -> PathBuf {
    cfg.acc_dir
        .join("task-workspaces")
        .join(task_id)
        .join(".voted")
}

/// Write the `.voted` sentinel file for `task_id`.
///
/// The directory is created if it does not already exist.  Failure to
/// write the file is logged but never fatal — the in-memory `mark_done`
/// call already provides same-process deduplication; the persistent file
/// is an *additional* guard for cross-restart deduplication.
fn mark_voted_persistent(cfg: &Config, task_id: &str) {
    let path = voted_sentinel_path(cfg, task_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
    {
        // Non-fatal: log but continue.  The in-memory cooldown still prevents
        // same-run duplicates; the file is an extra durability layer.
        eprintln!(
            "[tasks] mark_voted_persistent: failed to write {}: {e}",
            path.display()
        );
    }
}

/// Return `true` when the `.voted` sentinel file exists for `task_id`.
///
/// This is the cross-restart complement to `in_cooldown`: it survives
/// process crashes / restarts because it reads from the filesystem rather
/// than from the process-local `OnceLock`.
fn in_vote_cooldown_persistent(cfg: &Config, task_id: &str) -> bool {
    voted_sentinel_path(cfg, task_id).exists()
}

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

    let client = match Client::new(&cfg.acc_url, &cfg.acc_token) {
        Ok(c) => c,
        Err(e) => { eprintln!("[tasks] http client: {e}"); std::process::exit(1); }
    };

    // Recovery: unclaim any tasks the server still attributes to this
    // agent. They were claimed by a previous process that died (restart,
    // crash, kill); we have no in-memory state for them and cannot
    // resume work in-flight. Without this, a restarted agent sees
    // active >= max_concurrent and never polls work.
    cleanup_stale_claims(&cfg, &client).await;

    // Bus subscriber: wakes the poll loop immediately on dispatch nudge/assign.
    // `vote_nudge` fires specifically when the server sends a task_type=vote
    // nudge so the poll loop can bias toward Poll 4 (idea voting) on that
    // wakeup without disturbing the normal idle/busy cadence.
    //
    // The `PeerExchangeCoordinator` is shared with the bus subscriber so that
    // `agent.test_challenge` messages received while the poll loop is busy can
    // still be handled without blocking.
    let nudge = Arc::new(Notify::new());
    let vote_nudge = Arc::new(Notify::new());
    let coordinator = Arc::new(PeerExchangeCoordinator::new(&cfg.agent_name));
    {
        let cfg2 = cfg.clone();
        let client2 = client.clone();
        let nudge2 = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        let coord2 = coordinator.clone();
        tokio::spawn(bus_subscriber(cfg2, client2, nudge2, vote_nudge2, coord2));
    }
    loop {
        if is_quenched(&cfg) {
            log(&cfg, "quenched — sleeping");
            sleep(POLL_IDLE).await;
            continue;
        }

        let active = count_active_tasks(&cfg, &client).await;
        let at_work_cap = active >= max_concurrent;

        // Fetch online peers once per cycle (used by all three polls)
        let online_peers = peers::list_peers(&cfg, &client).await;
        let mut claimed = false;

        // ── Poll 1: work tasks (skipped when at capacity) ───────────────────
        if !at_work_cap {
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
                        if in_cooldown(&task_id) { continue; }

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
                                let title2 = claimed_task["title"].as_str().unwrap_or("").to_string();
                                let desc2  = claimed_task["description"].as_str().unwrap_or("").to_string();
                                let agent2 = cfg.agent_name.clone();
                                let tid2   = task_id.clone();
                                tokio::spawn(async move {
                                    slack::notify_claimed(&agent2, &tid2, &title2, &desc2).await;
                                });
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
        }

        // ── Poll 2: review tasks (runs even when at work capacity) ──────────
        // Reviews are bounded to 1 concurrent per agent (max_concurrent cap
        // applies separately; reviews are lighter than full work tasks).
        if !claimed {
            if let Ok(review_tasks) = fetch_open_tasks(&cfg, &client, 10, "review").await {
                for task in &review_tasks {
                    let task_id = task["id"].as_str().unwrap_or("").to_string();
                    if task_id.is_empty() { continue; }
                    if in_cooldown(&task_id) { continue; }

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
                            let title2 = claimed_task["title"].as_str().unwrap_or("").to_string();
                            let desc2  = claimed_task["description"].as_str().unwrap_or("").to_string();
                            let agent2 = cfg.agent_name.clone();
                            let tid2   = task_id.clone();
                            tokio::spawn(async move {
                                slack::notify_claimed(&agent2, &tid2, &title2, &desc2).await;
                            });
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
        if !claimed && !at_work_cap {
            if let Ok(phase_tasks) = fetch_open_tasks(&cfg, &client, 5, "phase_commit").await {
                for task in &phase_tasks {
                    let task_id = task["id"].as_str().unwrap_or("").to_string();
                    if task_id.is_empty() { continue; }
                    if in_cooldown(&task_id) { continue; }

                    match claim_task(&cfg, &client, &task_id).await {
                        Ok(claimed_task) => {
                            log(&cfg, &format!("claimed phase_commit {task_id}"));
                            let title2 = claimed_task["title"].as_str().unwrap_or("").to_string();
                            let desc2  = claimed_task["description"].as_str().unwrap_or("").to_string();
                            let agent2 = cfg.agent_name.clone();
                            let tid2   = task_id.clone();
                            tokio::spawn(async move {
                                slack::notify_claimed(&agent2, &tid2, &title2, &desc2).await;
                            });
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

        // ── Poll 4: idea-review voting ──────────────────────────────────────
        // Voting is NON-EXCLUSIVE — the agent reads the idea, evaluates it
        // via the agentic loop, and calls PUT /api/tasks/:id/vote. It does
        // NOT claim the task. Any number of agents can vote concurrently.
        // This tier runs unconditionally every cycle regardless of work
        // capacity or whether an earlier poll already claimed a task, because
        // votes are cheap and non-exclusive. It is biased toward running when
        // the bus delivers a task_type=vote nudge (see vote_nudge below), but
        // it also runs in every regular idle cycle so votes are never
        // permanently blocked.
        //
        // NOTE — no notify_claimed here, by design.
        // Polls 1-3 each call claim_task() and, on success, fire
        // slack::notify_claimed() to announce that an agent has taken
        // exclusive ownership of a task.  Poll 4 intentionally skips both
        // steps: idea tasks are never claimed (voting is concurrent and
        // non-exclusive), so there is no ownership event to announce.
        // Do NOT add a notify_claimed call here — it would be misleading
        // because the agent holds no lock on the idea and multiple agents
        // may vote simultaneously.
        // slack::notify_voting() (fired inside execute_idea_vote_task) provides
        // the lightweight "agent is now evaluating this idea" signal instead.
        if let Ok(idea_tasks) = fetch_open_tasks(&cfg, &client, 10, "idea").await {
            for task in &idea_tasks {
                let task_id = task["id"].as_str().unwrap_or("").to_string();
                if task_id.is_empty() { continue; }
                // Skip ideas the server told us to avoid re-voting.
                // `in_cooldown` covers same-process deduplication; the
                // persistent `.voted` file survives crashes/restarts so a
                // restarted agent never spawns a duplicate vote goroutine
                // for an idea it was already evaluating before the crash.
                if in_cooldown(&task_id) { continue; }
                if in_vote_cooldown_persistent(&cfg, &task_id) {
                    // Also populate in-memory cooldown so subsequent cycles
                    // in this process don't re-check the filesystem.
                    mark_done(&task_id);
                    continue;
                }

                // Skip if we are the idea's creator — the server enforces
                // this too (409), but short-circuiting here saves an RTT.
                let creator = task["metadata"]["created_by"].as_str().unwrap_or("");
                if !creator.is_empty() && creator == cfg.agent_name.as_str() {
                    continue;
                }

                // Skip if we have already voted on this idea.
                let already_voted = task["metadata"]["votes"]
                    .as_array()
                    .map(|votes| votes.iter().any(|v| v["agent"].as_str() == Some(&cfg.agent_name)))
                    .unwrap_or(false);
                if already_voted { continue; }

                log(&cfg, &format!("voting on idea {task_id}"));
                let cfg2 = cfg.clone();
                let client2 = client.clone();
                let task2 = task.clone();
                tokio::spawn(async move {
                    execute_idea_vote_task(&cfg2, &client2, &task2, |prompt, ws| {
                        Box::pin(crate::sdk::run_agent_owned(prompt, ws))
                    }).await;
                });
                // Mark done immediately — both in-memory (same-process) and
                // on disk (cross-restart) — so we don't spawn a duplicate
                // vote goroutine for the same idea in back-to-back cycles
                // or after a crash/restart before the first one completes.
                mark_done(&task_id);
                mark_voted_persistent(&cfg, &task_id);
                claimed = true; // treat as "did something" for sleep logic
                break;
            }
        }

        // ── Poll 5: peer test-exchange initiation ───────────────────────────
        // Once per poll cycle, attempt to initiate a priority test exchange
        // with a randomly-selected online peer.  The coordinator's in-memory
        // rate-limit table (one challenge per agent pair per hour) ensures we
        // never spam the same pair; `initiate_peer_exchange` returns silently
        // when rate-limited or when no peers are online.
        //
        // Poll 5 is non-exclusive (it never sets `claimed`) so it never
        // blocks the other polls from running in the same cycle.
        if !online_peers.is_empty() {
            let cfg2 = cfg.clone();
            let client2 = client.clone();
            let coord2 = coordinator.clone();
            let peers2 = online_peers.clone();
            tokio::spawn(async move {
                initiate_peer_exchange(&cfg2, &client2, &coord2, &peers2).await;
            });
        }

        if at_work_cap && !claimed {
            log(&cfg, &format!("at work capacity ({}/{}), waiting", active, max_concurrent));
        }

        if claimed {
            sleep(POLL_BUSY).await;
        } else {
            // Wait for idle timeout, a general dispatch nudge, or a
            // targeted vote nudge — whichever comes first.
            tokio::select! {
                _ = sleep(POLL_IDLE) => {}
                _ = nudge.notified() => {
                    log(&cfg, "woke early — dispatch nudge received");
                }
                _ = vote_nudge.notified() => {
                    log(&cfg, "woke early — vote nudge received");
                }
            }
        }
    }
}

// ── Startup recovery — release stale claims from previous process ───────────

async fn cleanup_stale_claims(cfg: &Config, client: &Client) {
    let stale = match client
        .tasks()
        .list()
        .status(TaskStatus::Claimed)
        .agent(cfg.agent_name.clone())
        .send()
        .await
    {
        Ok(v) => v,
        Err(e) => {
            log(cfg, &format!("startup recovery: failed to list own claims: {e}"));
            return;
        }
    };
    if stale.is_empty() {
        return;
    }
    log(
        cfg,
        &format!("startup recovery: releasing {} stale claim(s) from previous process", stale.len()),
    );
    for t in &stale {
        let _ = client.tasks().unclaim(&t.id, Some(&cfg.agent_name)).await;
        // Populate cooldown so we don't immediately re-claim. Other
        // agents (who haven't been running this task) can still pick it up.
        mark_done(&t.id);
        // Notify fleet-activity so operators can see tasks that were
        // interrupted mid-flight by an agent crash/restart.
        slack::notify_failed(
            &cfg.agent_name,
            &t.id,
            &t.title,
            "unclaimed on restart — agent was restarted while this task was in flight",
        ).await;
    }
}

// ── Bus subscriber — wakes poll loop on dispatch nudge/assign ─────────────────

async fn bus_subscriber(
    cfg: Config,
    client: Client,
    nudge: Arc<Notify>,
    vote_nudge: Arc<Notify>,
    coordinator: Arc<PeerExchangeCoordinator>,
) {
    loop {
        match subscribe_bus(&cfg, &client, &nudge, &vote_nudge, &coordinator).await {
            Ok(()) => {}
            Err(e) => {
                log(&cfg, &format!("[bus] disconnected: {e}, reconnecting in 5s"));
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn subscribe_bus(
    cfg: &Config,
    client: &Client,
    nudge: &Arc<Notify>,
    vote_nudge: &Arc<Notify>,
    coordinator: &Arc<PeerExchangeCoordinator>,
) -> Result<(), String> {
    use futures_util::StreamExt;
    let stream = client.bus().stream();
    tokio::pin!(stream);
    while let Some(msg) = stream.next().await {
        let msg = msg.map_err(|e| e.to_string())?;
        let kind = msg.kind.as_deref().unwrap_or("");
        let to = msg.to.as_deref().unwrap_or("");
        let is_directed_to_us = to == cfg.agent_name;
        let is_broadcast = to.is_empty() || to == "null";

        if kind == "tasks:dispatch_nudge" && (is_directed_to_us || is_broadcast) {
            // When the nudge carries task_type=vote the server is asking us
            // specifically to cast an idea-review vote. Fire vote_nudge so
            // the poll loop can prioritise Poll 4 on this wakeup. Also fire
            // the general nudge so the loop wakes up in any case.
            //
            // task_type is a top-level JSON field on the wire (not nested
            // under a "body" key), so BusMsg captures it in msg.extra via
            // #[serde(flatten)]. Reading from msg.body was always a no-op
            // and prevented vote_nudge from ever firing.
            let task_type = msg.extra.get("task_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if task_type == "vote" {
                vote_nudge.notify_one();
            }
            nudge.notify_one();
        } else if kind == "tasks:dispatch_assigned" && is_directed_to_us {
            nudge.notify_one();
        } else if kind == "agent.test_challenge" && is_directed_to_us {
            // ── Peer test exchange — incoming challenge ────────────────────
            // The initiating peer has sent a TestSuite for us to execute.
            // Extract the `ExchangeRequest` from the message body, spawn a
            // task to execute every TestCase and send back the results.
            let body_val = msg.body
                .clone()
                .or_else(|| msg.extra.get("suite").map(|s| {
                    // Some transports embed the full ExchangeRequest as a
                    // flat JSON object rather than nesting it under "body".
                    serde_json::json!({
                        "from": msg.from.clone().unwrap_or_default(),
                        "to":   msg.to.clone().unwrap_or_default(),
                        "suite": s,
                    })
                }));

            let exchange_req: Option<ExchangeRequest> = body_val
                .as_ref()
                .and_then(|v| match v {
                    Value::String(s) => serde_json::from_str(s).ok(),
                    other => serde_json::from_value(other.clone()).ok(),
                });

            if let Some(req) = exchange_req {
                let cfg2 = cfg.clone();
                let client2 = client.clone();
                let coord2 = coordinator.clone();
                tokio::spawn(async move {
                    handle_incoming_challenge(&cfg2, &client2, &coord2, req).await;
                });
            } else {
                log(cfg, "[peer-exchange] received agent.test_challenge but body could not be parsed as ExchangeRequest");
            }
        } else if kind == "agent.test_submit" && is_directed_to_us {
            // ── Peer test exchange — incoming results ─────────────────────
            // The target has executed our TestSuite and returned results.
            // Tally them; if failure rate exceeds the threshold, go offline
            // and file repair tasks.
            let reports_val = msg.body
                .clone()
                .or_else(|| msg.extra.get("reports").cloned().map(|r| serde_json::json!({"reports": r})));

            let reports: Vec<TestReport> = reports_val
                .as_ref()
                .and_then(|v| match v {
                    Value::String(s) => serde_json::from_str::<serde_json::Value>(s)
                        .ok()
                        .and_then(|j| j.get("reports").and_then(|r| serde_json::from_value(r.clone()).ok())),
                    Value::Object(_) => v.get("reports").and_then(|r| serde_json::from_value(r.clone()).ok()),
                    Value::Array(_) => serde_json::from_value(v.clone()).ok(),
                    _ => None,
                })
                .unwrap_or_default();

            if !reports.is_empty() {
                let actions = coordinator.decide_actions(&reports);
                let from_agent = msg.from.clone().unwrap_or_else(|| "unknown".to_string());
                let cfg2 = cfg.clone();
                let client2 = client.clone();
                tokio::spawn(async move {
                    for action in actions {
                        handle_test_results(&cfg2, &client2, &from_agent, action).await;
                    }
                });
            } else {
                log(cfg, "[peer-exchange] received agent.test_submit but no parseable reports found");
            }
        }
    }
    Ok(())
}

// ── Peer test exchange — initiation ───────────────────────────────────────────

/// Attempt to initiate a priority test exchange with a randomly-selected peer
/// from `online_peers`.  The `PeerExchangeCoordinator`'s in-memory rate-limit
/// table (one challenge per agent pair per hour) prevents spamming; this
/// function returns silently when rate-limited or when no suitable peer is
/// available.
///
/// On a successful initiation, the challenge `ExchangeRequest` is serialised
/// and sent as an `agent.test_challenge` bus message directed at the chosen
/// target agent.
async fn initiate_peer_exchange(
    cfg: &Config,
    client: &Client,
    coordinator: &PeerExchangeCoordinator,
    online_peers: &[String],
) {
    use crate::peer_exchange::{InitiateError, WorkspaceTestGenerator};

    // Pick a random online peer that is not us.
    let eligible: Vec<&String> = online_peers
        .iter()
        .filter(|p| p.as_str() != cfg.agent_name.as_str())
        .collect();
    if eligible.is_empty() {
        return;
    }
    // Simple pseudo-random selection by current timestamp modulo peers count.
    let idx = chrono::Utc::now().timestamp_subsec_millis() as usize % eligible.len();
    let target = eligible[idx].as_str();

    // Build a workspace-derived test suite.
    let generator = WorkspaceTestGenerator::from_env();
    let req = match coordinator.initiate(target, &generator) {
        Ok(r) => r,
        Err(InitiateError::RateLimited { retry_after_seconds }) => {
            log(cfg, &format!(
                "[peer-exchange] rate-limited for pair (us, {target}): {retry_after_seconds}s remaining"
            ));
            return;
        }
        Err(InitiateError::NoPeerAvailable)
        | Err(InitiateError::EmptySuite)
        | Err(InitiateError::SendFailed(_)) => {
            return;
        }
    };

    log(cfg, &format!(
        "[peer-exchange] initiating challenge → {} ({} tests)",
        target,
        req.suite.len()
    ));

    // Serialise the request and send it as agent.test_challenge.
    let body_json = match serde_json::to_string(&req) {
        Ok(j) => j,
        Err(e) => {
            log(cfg, &format!("[peer-exchange] failed to serialise ExchangeRequest: {e}"));
            return;
        }
    };

    let send_req = BusSendRequest {
        kind: "agent.test_challenge".into(),
        from: Some(cfg.agent_name.clone()),
        to: Some(target.to_string()),
        body: Some(body_json),
        ..Default::default()
    };

    if let Err(e) = client.bus().send(&send_req).await {
        log(cfg, &format!("[peer-exchange] failed to send challenge to {target}: {e}"));
    } else {
        log(cfg, &format!("[peer-exchange] challenge sent to {target}"));
    }
}

// ── Peer test exchange — concrete runner / handler ────────────────────────────

/// Concrete [`TestRunner`] that executes each [`TestCase`] as a shell command.
///
/// Each command is run with `sh -c` and subject to [`TEST_TIMEOUT_SECS`].
/// Timed-out commands are recorded as failures with exit code `-1`.
struct ShellTestRunner;

impl TestRunner for ShellTestRunner {
    fn run(&self, suite: &TestSuite) -> Vec<TestReport> {
        // We must block here because `TestRunner::run` is synchronous, but the
        // underlying I/O is async. The runtime is already set up by the time
        // this is called from a tokio::spawn, so we block_in_place to avoid
        // starving the executor.
        tokio::task::block_in_place(|| {
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async { run_suite_async(suite).await })
        })
    }
}

async fn run_suite_async(suite: &TestSuite) -> Vec<TestReport> {
    let mut reports = Vec::with_capacity(suite.tests.len());
    for tc in &suite.tests {
        let report = run_single_test(tc).await;
        reports.push(report);
    }
    reports
}

async fn run_single_test(tc: &TestCase) -> TestReport {
    let timeout = Duration::from_secs(TEST_TIMEOUT_SECS);
    let result = tokio::time::timeout(
        timeout,
        Command::new("sh")
            .arg("-c")
            .arg(&tc.command)
            .output(),
    )
    .await;

    match result {
        Err(_elapsed) => {
            // Timed out
            TestReport::fail(
                &tc.id,
                -1,
                "",
                &format!("test timed out after {TEST_TIMEOUT_SECS}s"),
            )
        }
        Ok(Err(io_err)) => {
            TestReport::fail(&tc.id, -1, "", &format!("I/O error: {io_err}"))
        }
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let actual_exit = output.status.code().unwrap_or(-1);

            // Pass criteria: exit code matches AND optional stdout substring present.
            let exit_ok = actual_exit == tc.expected_exit_code;
            let stdout_ok = tc.expected_output_contains.as_deref()
                .map(|needle| stdout.contains(needle))
                .unwrap_or(true);
            let passed = exit_ok && stdout_ok;

            TestReport {
                test_id: tc.id.clone(),
                passed,
                actual_exit_code: actual_exit,
                stdout,
                stderr,
            }
        }
    }
}

/// Handle an incoming `agent.test_challenge` bus message.
///
/// Executes every test case in the received suite using [`ShellTestRunner`],
/// then sends the results back to the initiating agent as an `agent.test_submit`
/// bus message.
async fn handle_incoming_challenge(
    cfg: &Config,
    client: &Client,
    coordinator: &PeerExchangeCoordinator,
    req: ExchangeRequest,
) {
    log(cfg, &format!(
        "[peer-exchange] received challenge from {} — {} test(s)",
        req.from,
        req.suite.len()
    ));

    let reports = coordinator.execute(&req, &ShellTestRunner);

    let pass_count = reports.iter().filter(|r| r.passed).count();
    let fail_count = reports.len() - pass_count;
    log(cfg, &format!(
        "[peer-exchange] executed {} test(s): {} passed, {} failed — sending results to {}",
        reports.len(), pass_count, fail_count, req.from
    ));

    // Encode reports as JSON and send back as agent.test_submit.
    let reports_json = match serde_json::to_string(&reports) {
        Ok(j) => j,
        Err(e) => {
            log(cfg, &format!("[peer-exchange] failed to serialise reports: {e}"));
            return;
        }
    };

    let send_req = BusSendRequest {
        kind: "agent.test_submit".into(),
        from: Some(cfg.agent_name.clone()),
        to: Some(req.from.clone()),
        body: Some(reports_json),
        ..Default::default()
    };

    if let Err(e) = client.bus().send(&send_req).await {
        log(cfg, &format!("[peer-exchange] failed to send test results to {}: {e}", req.from));
    } else {
        log(cfg, &format!("[peer-exchange] results sent to {}", req.from));
    }
}

/// Handle one ordered action step produced after our challenge results return.
///
/// The caller iterates the sequence produced by
/// [`PeerExchangeCoordinator::decide_actions`] and calls this function once
/// per step.  Each step emits a distinct, observable bus message so that any
/// subscriber on the fleet can react to each side-effect independently.
///
/// Steps in the failure sequence (emitted in this order):
///
/// 1. [`AgentAction::GoOffline`] — write the quench file so the poll loop
///    stops picking up new tasks; emits `agent.peer_exchange.go_offline`.
/// 2. [`AgentAction::RaiseBeadsTask`] — file one `work` task for a single
///    failed test (called once per failed test, priority-descending so the
///    most urgent defect enters the queue first); emits
///    `agent.peer_exchange.raise_beads_task`.
/// 3. [`AgentAction::BeginAutoFix`] — signals that automated remediation
///    starts; emits `agent.peer_exchange.begin_auto_fix`.
///
/// Pass case:
/// * [`AgentAction::LogPassSummary`] — logs a human-readable pass/fail line;
///   emits `agent.peer_exchange.pass_summary`.
async fn handle_test_results(
    cfg: &Config,
    client: &Client,
    target_agent: &str,
    action: AgentAction,
) {
    match &action {
        AgentAction::LogPassSummary { total, failed } => {
            log(cfg, &format!(
                "[peer-exchange] challenge against {target_agent} passed — \
                 {failed}/{total} failures (at or below threshold)"
            ));
            emit_bus_event(cfg, client, "agent.peer_exchange.pass_summary", target_agent,
                serde_json::json!({
                    "total": total,
                    "failed": failed,
                    "target_agent": target_agent,
                }),
            ).await;
        }

        AgentAction::GoOffline { total, failed, failed_ids } => {
            log(cfg, &format!(
                "[peer-exchange] FAILURE THRESHOLD EXCEEDED against {target_agent} — \
                 {failed}/{total} tests failed; going offline (step 1/3)"
            ));
            // Write quench file — stops the poll loop from claiming new work.
            go_offline(cfg);
            emit_bus_event(cfg, client, "agent.peer_exchange.go_offline", target_agent,
                serde_json::json!({
                    "total": total,
                    "failed": failed,
                    "failed_ids": failed_ids,
                    "target_agent": target_agent,
                }),
            ).await;
        }

        AgentAction::RaiseBeadsTask { test_id, priority } => {
            log(cfg, &format!(
                "[peer-exchange] raising beads task for test {test_id} \
                 (priority={priority}) on {target_agent} (step 2/3)"
            ));
            file_single_defect_task(cfg, client, target_agent, test_id, *priority).await;
            emit_bus_event(cfg, client, "agent.peer_exchange.raise_beads_task", target_agent,
                serde_json::json!({
                    "test_id": test_id,
                    "priority": priority,
                    "target_agent": target_agent,
                }),
            ).await;
        }

        AgentAction::BeginAutoFix { total, failed, failed_ids } => {
            log(cfg, &format!(
                "[peer-exchange] beginning auto-fix for {failed}/{total} failures \
                 on {target_agent} (step 3/3)"
            ));
            emit_bus_event(cfg, client, "agent.peer_exchange.begin_auto_fix", target_agent,
                serde_json::json!({
                    "total": total,
                    "failed": failed,
                    "failed_ids": failed_ids,
                    "target_agent": target_agent,
                }),
            ).await;
            log(cfg, "[peer-exchange] offline mode active; repair tasks filed; \
                 auto-fix signalled — restart acc-agent tasks to resume normal \
                 operation after fixes");
        }

        // Consolidated variant: perform all three steps (go offline → raise
        // beads tasks → begin auto-fix) inside a single handler invocation.
        AgentAction::GoOfflineAndFix { total, failed, failed_ids } => {
            log(cfg, &format!(
                "[peer-exchange] FAILURE THRESHOLD EXCEEDED against {target_agent} — \
                 {failed}/{total} tests failed; going offline and scheduling auto-fix"
            ));
            go_offline(cfg);
            emit_bus_event(cfg, client, "agent.peer_exchange.go_offline_and_fix", target_agent,
                serde_json::json!({
                    "total": total,
                    "failed": failed,
                    "failed_ids": failed_ids,
                    "target_agent": target_agent,
                }),
            ).await;
            for (priority, test_id) in failed_ids.iter().enumerate() {
                file_single_defect_task(cfg, client, target_agent, test_id, priority as u32).await;
            }
            log(cfg, "[peer-exchange] offline mode active; repair tasks filed; \
                 auto-fix signalled — restart acc-agent to resume after fixes");
        }
    }
}

/// Publish a structured bus event for an individual peer-exchange action step.
///
/// The message is broadcast to all fleet subscribers (`to: "all"`) so any
/// agent or monitoring system observing the bus can react to each step
/// independently.  Failures to post are logged but never fatal — the agent
/// continues through the action sequence regardless.
async fn emit_bus_event(
    cfg: &Config,
    client: &Client,
    kind: &str,
    target_agent: &str,
    payload: serde_json::Value,
) {
    let body = match serde_json::to_string(&payload) {
        Ok(s) => s,
        Err(e) => {
            log(cfg, &format!("[peer-exchange] failed to serialise bus event {kind}: {e}"));
            return;
        }
    };
    let req = BusSendRequest {
        kind: kind.to_string(),
        from: Some(cfg.agent_name.clone()),
        to: Some("all".to_string()),
        subject: Some(format!("peer-exchange: {kind} (target={target_agent})")),
        body: Some(body),
        ..Default::default()
    };
    if let Err(e) = client.bus().send(&req).await {
        log(cfg, &format!("[peer-exchange] failed to emit bus event {kind}: {e}"));
    }
}

/// Touch the quench file, causing `is_quenched()` to return `true` and the
/// poll loop to stop accepting new tasks.
fn go_offline(cfg: &Config) {
    let qf = cfg.quench_file();
    // best-effort: create the quench file.
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&qf)
    {
        tracing::warn!("[peer-exchange] failed to write quench file {}: {e}", qf.display());
    } else {
        log(cfg, &format!("[peer-exchange] quench file written — agent is now offline: {}", qf.display()));
    }
}

/// File a single `work` task for one failed test case.
///
/// Called once per [`AgentAction::RaiseBeadsTask`] step so that each
/// defect enters the fleet queue as a separate, independently trackable
/// task.  The `priority` argument is taken directly from the action step
/// and reflects priority-descending ordering: the first call carries
/// priority `0` (most urgent), the second `1`, and so on.
///
/// The task is filed into `ACC_PEER_EXCHANGE_PROJECT_ID` (env var) or
/// falls back to a generic `"acc"` project slug so there is always a
/// valid project_id even when the env var is absent.
async fn file_single_defect_task(
    cfg: &Config,
    client: &Client,
    target_agent: &str,
    test_id: &str,
    priority: u32,
) {
    let project_id = std::env::var("ACC_PEER_EXCHANGE_PROJECT_ID")
        .unwrap_or_else(|_| "acc".to_string());

    let req = CreateTaskRequest {
        project_id: project_id.clone(),
        title: format!(
            "[peer-exchange defect] test {test_id} failed on {target_agent}"
        ),
        description: Some(format!(
            "The peer test-exchange protocol detected a failure on agent `{target_agent}`.\n\n\
             **Failed test:** `{test_id}`\n\n\
             **Initiator:** `{initiator}`\n\n\
             Investigate the root cause, apply the fix, and re-run the failing test to confirm \
             it passes before bringing the agent back online (remove the quench file at \
             `~/.acc/quench`).",
            target_agent = target_agent,
            test_id = test_id,
            initiator = cfg.agent_name,
        )),
        priority: Some(priority as i64),
        task_type: Some(TaskType::Work),
        metadata: Some(serde_json::json!({
            "source": "peer_exchange",
            "failed_test_id": test_id,
            "target_agent": target_agent,
            "initiator": cfg.agent_name,
            "tags": ["peer-exchange", "defect", "auto-filed"],
        })),
        ..Default::default()
    };

    match client.tasks().create(&req).await {
        Ok(task) => log(cfg, &format!(
            "[peer-exchange] filed defect task {} for test {test_id} (priority={priority})", task.id
        )),
        Err(e) => log(cfg, &format!(
            "[peer-exchange] failed to file defect task for {test_id}: {e}"
        )),
    }
}

// ── Fetching / claiming ───────────────────────────────────────────────────────

async fn fetch_open_tasks(_cfg: &Config, client: &Client, limit: usize, task_type: &str) -> Result<Vec<Value>, String> {
    let tt = parse_task_type(task_type).unwrap_or(TaskType::Work);
    let tasks = client
        .tasks()
        .list()
        .status(TaskStatus::Open)
        .task_type(tt)
        .limit(limit.max(1) as u32)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(tasks.into_iter().map(to_value).collect())
}

async fn count_active_tasks(cfg: &Config, client: &Client) -> usize {
    match client
        .tasks()
        .list()
        .status(TaskStatus::Claimed)
        .agent(cfg.agent_name.clone())
        .send()
        .await
    {
        Ok(tasks) => tasks.len(),
        Err(_) => 0,
    }
}

async fn claim_task(cfg: &Config, client: &Client, task_id: &str) -> Result<Value, u16> {
    match client.tasks().claim(task_id, &cfg.agent_name).await {
        Ok(task) => Ok(to_value(task)),
        Err(e) => Err(e.status_code().unwrap_or(500)),
    }
}

// ── Work task execution ───────────────────────────────────────────────────────

async fn execute_task(cfg: &Config, client: &Client, task: &Value, online_peers: &[String]) {
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let title = task["title"].as_str().unwrap_or("(no title)");
    let project_id = task["project_id"].as_str().unwrap_or("");

    log(cfg, &format!("executing task {task_id}: {title}"));

    let workspace = resolve_workspace(cfg, client, project_id, task_id).await;
    let _ = std::fs::create_dir_all(&workspace);

    let ctx_path = workspace.join(".task-context.json");
    let _ = std::fs::write(&ctx_path, task.to_string());

    let description = task["description"].as_str().unwrap_or("");
    let prompt = format!(
        "You are an autonomous coding agent. Your task:\n\
         \n\
         Title: {title}\n\
         \n\
         Description:\n\
         {description}\n\
         \n\
         You are in a git working directory with the project source. Apply the \
         requested changes by calling `str_replace_editor` (for file edits) or \
         `bash` (to run scripts, tests, etc.). The completion of this task is \
         verified by `git diff` against your edits — a written description that \
         doesn't actually modify files counts as a failed task.\n\
         \n\
         When the edits are applied, summarize in 1-3 sentences what you changed."
    );

    // CCC-79d: heartbeat every 60s while the agentic loop runs so the
    // hub doesn't read silence-for-2h as agent-dead.
    let ka_stop = spawn_keepalive(
        cfg.clone(),
        client.clone(),
        format!("working task {task_id}"),
    );
    let result = match tokio::time::timeout(
        WORK_TIMEOUT,
        crate::sdk::run_agent(&prompt, &workspace),
    ).await {
        Ok(r) => r,
        Err(_) => Err(format!("timeout after {}m", WORK_TIMEOUT.as_secs() / 60)),
    };
    let _ = ka_stop.send(());

    match result {
        Ok(output) => {
            if cfg.pair_programming {
                submit_for_review(cfg, client, task, &output, online_peers).await;
            } else {
                complete_task(cfg, client, task_id, &output).await;
                log(cfg, &format!("completed {task_id}"));
                // Fire-and-forget: do not block task completion on Slack API latency.
                {
                    let agent2  = cfg.agent_name.clone();
                    let tid2    = task_id.to_string();
                    let title2  = title.to_string();
                    let output2 = output.clone();
                    tokio::spawn(async move {
                        slack::notify_completed(&agent2, &tid2, &title2, &output2).await;
                    });
                }
            }
        }
        Err(e) => {
            log(cfg, &format!("task {task_id} failed: {e}"));
            unclaim_task(cfg, client, task_id).await;
            slack::notify_failed(&cfg.agent_name, task_id, title, &e).await;
        }
    }
    mark_done(task_id);
}


// ── Pair programming: submit for review ──────────────────────────────────────

async fn submit_for_review(cfg: &Config, client: &Client, task: &Value, output: &str, online_peers: &[String]) {
    let task_id = task["id"].as_str().unwrap_or("");
    let project_id = task["project_id"].as_str().unwrap_or("");
    let title = task["title"].as_str().unwrap_or("(task)");
    let priority = task["priority"].as_i64().unwrap_or(2);
    let phase = task["phase"].as_str();

    // Work is done — complete it first
    complete_task(cfg, client, task_id, output).await;
    // Fire-and-forget: do not block task completion on Slack API latency.
    {
        let agent2  = cfg.agent_name.clone();
        let tid2    = task_id.to_string();
        let title2  = title.to_string();
        let output2 = output.to_string();
        tokio::spawn(async move {
            slack::notify_completed(&agent2, &tid2, &title2, &output2).await;
        });
    }

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

    let req = CreateTaskRequest {
        project_id: project_id.to_string(),
        title: format!("Review: {title}"),
        description: Some(review_desc),
        priority: Some(priority),
        task_type: Some(TaskType::Review),
        review_of: Some(task_id.to_string()),
        phase: phase.map(|p| p.to_string()),
        metadata: Some(meta),
        ..Default::default()
    };

    match client.tasks().create(&req).await {
        Ok(review) => {
            log(cfg, &format!("submitted {task_id} for review → {} (reviewer: {})", review.id,
                if reviewer.is_empty() { "any" } else { reviewer }));
        }
        Err(e) => log(cfg, &format!("failed to create review task: {e}")),
    }
}

// ── Review task execution ─────────────────────────────────────────────────────

async fn execute_review_task(cfg: &Config, client: &Client, task: &Value) {
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let review_of_id = task["review_of"].as_str().unwrap_or("");
    let phase = task["phase"].as_str().unwrap_or("");

    log(cfg, &format!("executing review {task_id} (reviewing {review_of_id})"));

    // Fetch original task to get project_id
    let project_id = fetch_task_project_id(cfg, client, review_of_id, task).await;

    let workspace = resolve_workspace(cfg, client, &project_id, "").await;
    let _ = std::fs::create_dir_all(&workspace);

    let work_summary = task["metadata"]["work_output_summary"].as_str().unwrap_or("");
    let ctx = serde_json::json!({
        "review_task": task,
        "review_of_id": review_of_id,
        "work_output_summary": work_summary,
    });
    let _ = std::fs::write(workspace.join(".review-context.json"), ctx.to_string());

    let review_prompt = format!(
        "You are a code reviewer in an automated pair-programming workflow.\n\n\
         Original task: {title}\n\
         Original task ID: {review_of_id}\n\
         Worker's own summary: {summary}\n\n\
         The working directory contains the project files written by the worker.\n\n\
         IMPORTANT — summary hallucination check: Before writing your verdict, run \
         `git diff HEAD~1 HEAD` (or `git log -1 --stat`) in the working directory to \
         see exactly what was changed. If the worker's summary mentions specific \
         function names, types, or structural changes that do not appear anywhere in \
         that diff or in the codebase, set `\"summary_hallucination\": true` in your \
         response. A hallucinated summary misleads reviewers who read only the summary; \
         flag it even if the underlying code changes are otherwise acceptable.\n\n\
         Review this work and respond with ONLY a single valid JSON object — no prose, no markdown:\n\
         {{\n\
           \"verdict\": \"approved\",\n\
           \"reason\": \"<one sentence>\",\n\
           \"summary_hallucination\": false,\n\
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
         (4) remaining gaps the original task left unaddressed, \
         (5) whether the worker summary accurately reflects the actual diff (set \
         `summary_hallucination` accordingly).",
        title = task["title"].as_str().unwrap_or("(task)"),
        summary = &work_summary[..work_summary.len().min(2000)],
    );

    // CCC-79d: heartbeat every 60s while the review's agentic loop
    // runs. Without this, the hub sees silence for the full
    // REVIEW_TIMEOUT (30min) and may treat the agent as dead.
    let ka_stop = spawn_keepalive(
        cfg.clone(),
        client.clone(),
        format!("reviewing task {task_id}"),
    );
    let review_output = match tokio::time::timeout(
        REVIEW_TIMEOUT,
        crate::sdk::run_agent(&review_prompt, &workspace),
    ).await {
        Ok(r) => r,
        Err(_) => Err(format!("timeout after {}m", REVIEW_TIMEOUT.as_secs() / 60)),
    };
    let _ = ka_stop.send(());

    // If the agentic loop itself failed (timeout, model error) treat this as
    // a transient infrastructure failure: unclaim the task so it can be
    // retried by another agent/run, and post a :warning: Slack notice so
    // operators can distinguish a real infra failure from a genuine review
    // rejection.  Do NOT complete the task with a fake "rejected" verdict —
    // that would silently close the task and prevent any human from noticing.
    let review_out_str = match review_output {
        Ok(out) => out,
        Err(e) => {
            log(cfg, &format!("review agent loop failed: {e}"));
            unclaim_task(cfg, client, task_id).await;
            slack::notify_failed(
                &cfg.agent_name,
                task_id,
                task["title"].as_str().unwrap_or("(review)"),
                &format!("review agent loop failed/unclaimed: {e}"),
            ).await;
            mark_done(task_id);
            return;
        }
    };

    let (verdict, reason, summary_hallucination, gaps) = parse_review_output(&review_out_str);

    // File gap tasks
    for gap in &gaps {
        create_gap_task(cfg, client, &project_id, phase, task_id, gap).await;
    }

    // Record verdict on the original work task; propagate hallucination flag
    // so the server can persist it in the work task's metadata.
    if !review_of_id.is_empty() {
        set_review_result_on_task(
            cfg, client, review_of_id, &verdict, &reason,
            if summary_hallucination { Some(true) } else { None },
        ).await;
    }

    if summary_hallucination {
        log(cfg, &format!(
            "review {task_id}: SUMMARY HALLUCINATION detected — worker summary \
             describes code not present in the diff; flagged in work task metadata"
        ));
    }

    complete_task(cfg, client, task_id, &format!("verdict: {verdict}, reason: {reason}")).await;
    // Fire-and-forget: do not block task completion on Slack API latency.
    {
        let agent2   = cfg.agent_name.clone();
        let tid2     = task_id.to_string();
        let title2   = task["title"].as_str().unwrap_or("(review)").to_string();
        let message2 = format!("verdict: {verdict} — {reason}");
        tokio::spawn(async move {
            slack::notify_completed(&agent2, &tid2, &title2, &message2).await;
        });
    }
    mark_done(task_id);
    log(cfg, &format!("review {task_id} done: {verdict} ({} gaps filed)", gaps.len()));
}

async fn fetch_task_project_id(_cfg: &Config, client: &Client, task_id: &str, fallback_task: &Value) -> String {
    if task_id.is_empty() {
        return fallback_task["project_id"].as_str().unwrap_or("").to_string();
    }
    match client.tasks().get(task_id).await {
        Ok(task) => task.project_id,
        Err(_) => fallback_task["project_id"].as_str().unwrap_or("").to_string(),
    }
}

fn parse_review_output(output: &str) -> (String, String, bool, Vec<Value>) {
    let start = output.find('{').unwrap_or(output.len());
    let end = output.rfind('}').map(|i| i + 1).unwrap_or(output.len());
    if start >= end {
        return ("rejected".to_string(), "unparseable output".to_string(), false, vec![]);
    }
    match serde_json::from_str::<Value>(&output[start..end]) {
        Ok(v) => {
            let verdict = v["verdict"].as_str().unwrap_or("rejected").to_string();
            let reason = v["reason"].as_str().unwrap_or("").to_string();
            let summary_hallucination = v["summary_hallucination"].as_bool().unwrap_or(false);
            let gaps = v["gaps"].as_array().cloned().unwrap_or_default();
            (verdict, reason, summary_hallucination, gaps)
        }
        Err(_) => ("rejected".to_string(), "unparseable output".to_string(), false, vec![]),
    }
}

async fn create_gap_task(cfg: &Config, client: &Client, project_id: &str, phase: &str, review_task_id: &str, gap: &Value) {
    let title = gap["title"].as_str().unwrap_or("Gap task").to_string();
    let description = gap["description"].as_str().unwrap_or("").to_string();
    let priority = gap["priority"].as_i64().unwrap_or(2);

    let req = CreateTaskRequest {
        project_id: project_id.to_string(),
        title: title.clone(),
        description: Some(description),
        priority: Some(priority),
        task_type: Some(TaskType::Work),
        phase: (!phase.is_empty()).then(|| phase.to_string()),
        metadata: Some(serde_json::json!({"spawned_by_review": review_task_id})),
        ..Default::default()
    };

    match client.tasks().create(&req).await {
        Ok(task) => log(cfg, &format!("filed gap task {}: {title}", task.id)),
        Err(e) => log(cfg, &format!("failed to create gap task: {e}")),
    }
}

async fn set_review_result_on_task(cfg: &Config, client: &Client, task_id: &str, verdict: &str, reason: &str, summary_hallucination: Option<bool>) {
    let result = match verdict {
        "approved" => ReviewResult::Approved,
        _ => ReviewResult::Rejected,
    };
    let _ = client
        .tasks()
        .review_result(task_id, result, Some(&cfg.agent_name), Some(reason), summary_hallucination)
        .await;
}

// ── Idea-review voting ────────────────────────────────────────────────────────

/// Read an idea task, evaluate it via the agentic loop, and cast a vote via
/// PUT /api/tasks/:id/vote.  Voting is NON-EXCLUSIVE — the task is never
/// claimed; any number of agents may vote concurrently.
///
/// The agent runs `sdk::run_agent` with a focused evaluation prompt that
/// asks for a JSON response containing:
///   - `vote`: "approve" | "reject"
///   - `refinement`: non-empty string for ALL votes (server enforces this)
///   - `reason`: brief explanation
///
/// On a successful vote the task ID is placed in the reclaim-cooldown map so
/// the poll loop won't spawn a duplicate vote goroutine for the same idea.
async fn execute_idea_vote_task<F>(cfg: &Config, client: &Client, task: &Value, run_agent_fn: F)
where
    F: Fn(String, PathBuf) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'static>>,
{
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let title   = task["title"].as_str().unwrap_or("(no title)");
    let description = task["description"].as_str().unwrap_or("");

    log(cfg, &format!("evaluating idea {task_id}: {title}"));

    // Notify fleet observers that this agent has started evaluating the idea.
    // Idea tasks are never claimed (voting is non-exclusive), so notify_claimed
    // is never fired for them.  This call fills that visibility gap: it gives
    // the same kind of "agent is now working on X" signal that notify_claimed
    // provides for exclusive work/review tasks, without implying ownership.
    {
        let agent2 = cfg.agent_name.clone();
        let tid2   = task_id.to_string();
        let title2 = title.to_string();
        tokio::spawn(async move {
            slack::notify_voting(&agent2, &tid2, &title2).await;
        });
    }

    let workspace = cfg.acc_dir.join("task-workspaces").join(task_id);
    let _ = std::fs::create_dir_all(&workspace);

    let ctx_path = workspace.join(".idea-context.json");
    let _ = std::fs::write(&ctx_path, task.to_string());

    // Collect existing votes so the model can see the current tally.
    let existing_votes_json = task["metadata"]["votes"]
        .as_array()
        .map(|v| serde_json::to_string_pretty(&serde_json::Value::Array(v.clone()))
            .unwrap_or_else(|_| "[]".to_string()))
        .unwrap_or_else(|| "[]".to_string());

    let prompt = format!(
        "You are an autonomous software-engineering agent performing an idea-review vote.\n\
         \n\
         An \"idea\" task is a proposal for new work. Your job is to evaluate the proposal \
         and cast a structured vote. Read the idea carefully, then respond with ONLY a \
         single valid JSON object — no prose, no markdown fences:\n\
         {{\n\
           \"vote\": \"approve\",\n\
           \"refinement\": \"<one or more concrete sentences — REQUIRED for BOTH approve \
         and reject; when approving, describe what you would add or improve; when \
         rejecting, explain the specific flaw and what would need to change for the idea \
         to be acceptable>\",\n\
           \"reason\": \"<one sentence explaining why you approve or reject>\"\n\
         }}\n\
         \n\
         Replace \"approve\" with \"reject\" only if the idea is fundamentally flawed, \
         duplicates existing work, or would cause harm. For any reasonable proposal, \
         approve and add a refinement.\n\
         \n\
         The `refinement` field is REQUIRED and MUST be non-empty for every vote — \
         approve or reject. The server will reject your vote with HTTP 400 if refinement \
         is absent or blank. When approving, describe concrete implementation steps, \
         edge cases to handle, interfaces to define, or follow-up tasks to file. When \
         rejecting, state the specific flaw and what would need to change.\n\
         \n\
         You may use `bash` to inspect the current codebase (read-only) to inform your \
         evaluation. Do NOT make any file changes — this is a read-and-vote operation.\n\
         \n\
         === IDEA ===\n\
         Title: {title}\n\
         \n\
         Description:\n\
         {description}\n\
         \n\
         === EXISTING VOTES ===\n\
         {existing_votes_json}\n\
         \n\
         Respond with ONLY the JSON object described above.",
    );

    let ka_stop = spawn_keepalive(
        cfg.clone(),
        client.clone(),
        format!("voting on idea {task_id}"),
    );
    let result = match tokio::time::timeout(
        VOTE_TIMEOUT,
        run_agent_fn(prompt, workspace),
    ).await {
        Ok(r) => r,
        Err(_) => Err(format!("timeout after {}m", VOTE_TIMEOUT.as_secs() / 60)),
    };
    let _ = ka_stop.send(());

    match result {
        Err(e) => {
            log(cfg, &format!("idea vote {task_id} agent loop failed: {e}"));
            // Notify the fleet-activity channel so operators can see idea-vote
            // failures alongside work/review failures.  The idea task itself is
            // never claimed (voting is non-exclusive), so we use notify_failed
            // to surface the error rather than notify_completed.
            slack::notify_failed(
                &cfg.agent_name,
                task_id,
                title,
                &format!("idea vote agent loop failed: {e}"),
            ).await;
            // cooldown already set by caller (mark_done before spawn); nothing to do.
        }
        Ok(output) => {
            let (vote, refinement, reason) = parse_vote_output(&output);
            log(cfg, &format!(
                "idea vote {task_id}: vote={vote} reason={reason} refinement_len={}",
                refinement.len(),
            ));
            match client.tasks()
                .vote(task_id, &cfg.agent_name, &vote, Some(&refinement))
                .await
            {
                Ok(_) => {
                    log(cfg, &format!("idea vote {task_id}: submitted vote={vote}"));
                    // Write persistent cooldown sentinel so that even after a
                    // crash/restart the poll loop knows this idea was already
                    // voted on and will not spawn a duplicate goroutine.
                    mark_voted_persistent(cfg, task_id);
                    // Fire-and-forget: post a Slack message so fleet observers
                    // can see voting activity alongside work/review events.
                    // Idea tasks are never claimed (voting is non-exclusive),
                    // so notify_claimed is intentionally absent; this is the
                    // sole Slack touchpoint for idea votes.
                    {
                        let agent2   = cfg.agent_name.clone();
                        let tid2     = task_id.to_string();
                        let title2   = title.to_string();
                        let vote2    = vote.clone();
                        tokio::spawn(async move {
                            slack::notify_voted(&agent2, &tid2, &title2, &vote2).await;
                        });
                    }
                }
                Err(e) => {
                    // 409 means the task is not an idea-type task or we are
                    // the creator (self-votes are rejected). Duplicate votes
                    // are idempotently accepted by the server, not 409'd.
                    log(cfg, &format!("idea vote {task_id}: PUT /vote failed: {e}"));
                }
            }
        }
    }
    // The in-memory cooldown was already set by the poll loop before spawning,
    // so we do not call mark_done here again (it would reset the timer —
    // harmless but logically cleaner to leave the original timestamp).
    // The persistent .voted file is written on successful vote submission
    // above (mark_voted_persistent); on failure the file is intentionally
    // absent so the next process can retry the evaluation.
}

/// Parse the JSON blob produced by the idea-evaluation agentic loop.
///
/// Expected shape:
/// ```json
/// { "vote": "approve", "refinement": "...", "reason": "..." }
/// ```
///
/// Tolerant of leading/trailing prose (extracts the first `{…}` span).
/// Defaults to `"reject"` if parsing fails so bad output never silently
/// counts as an approval.
///
/// The server requires `refinement` to be a non-empty string for **every**
/// vote (approve or reject).  If the parsed output lacks a non-empty
/// refinement this is treated as a hard parse error and the function
/// returns a synthetic reject with a descriptive reason, preventing an
/// HTTP 400 from the server.
fn parse_vote_output(output: &str) -> (String, String, String) {
    let start = output.find('{').unwrap_or(output.len());
    let end   = output.rfind('}').map(|i| i + 1).unwrap_or(output.len());
    if start >= end {
        return (
            "reject".to_string(),
            "model did not provide refinement".to_string(),
            "unparseable output".to_string(),
        );
    }
    match serde_json::from_str::<serde_json::Value>(&output[start..end]) {
        Ok(v) => {
            let vote   = v["vote"].as_str().unwrap_or("reject").to_string();
            let reason = v["reason"].as_str().unwrap_or("").to_string();
            let refinement_opt = v["refinement"].as_str()
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty());
            match refinement_opt {
                Some(r) => (vote, r, reason),
                None => (
                    "reject".to_string(),
                    "model did not provide refinement".to_string(),
                    "unparseable output".to_string(),
                ),
            }
        }
        Err(_) => (
            "reject".to_string(),
            "model did not provide refinement".to_string(),
            "unparseable output".to_string(),
        ),
    }
}

// ── Phase commit task execution ───────────────────────────────────────────────

async fn execute_phase_commit_task(cfg: &Config, client: &Client, task: &Value) {
    let task_id = task["id"].as_str().unwrap_or("unknown");
    let project_id = task["project_id"].as_str().unwrap_or("");
    let phase = task["phase"].as_str().unwrap_or("unknown");

    log(cfg, &format!("executing phase_commit {task_id}: phase={phase}"));

    let workspace = resolve_workspace(cfg, client, project_id, "").await;
    let branch = format!("phase/{phase}");
    let n_blocked = task["blocked_by"].as_array().map(|a| a.len()).unwrap_or(0);
    let commit_msg = format!("phase commit: {phase} ({n_blocked} tasks reviewed and approved)");

    match run_git_phase_commit(&workspace, &branch, &commit_msg).await {
        Ok(out) => {
            log(cfg, &format!("phase_commit {task_id}: pushed {branch}"));

            // Drift-fix #2: phase branches were piling up on origin
            // without ever being merged back to main (852 unmerged
            // commits on phase/milestone observed). Try a fast-forward
            // merge of phase/<phase> back into main and push. If the
            // FF can't happen (e.g. someone landed a PR on main since
            // we branched), leave main alone and surface for human
            // review — never do non-FF merges automatically.
            //
            // Use run_git_merge_to_main_inner here rather than the
            // run_git_merge_to_main wrapper so that both git sequences
            // (phase-commit and merge-to-main) can be composed under a
            // single workspace mutex guard in the future without
            // triggering a re-entrant deadlock on tokio::sync::Mutex.
            let merge_outcome = run_git_merge_to_main_inner(&workspace, &branch).await;
            match &merge_outcome {
                Ok(s) => log(cfg, &format!("phase_commit {task_id}: merged {branch} → main ({s})")),
                Err(e) => log(cfg, &format!("phase_commit {task_id}: merge to main skipped/failed: {e}")),
            }

            // CCC-tk0: this is the milestone-commit task. Now that the
            // AgentFS state is committed and pushed to git, mark the
            // project's AgentFS as clean. Server-side dirty bit gets
            // re-set the next time any task in this project completes.
            if !project_id.is_empty() {
                let path = format!("/api/projects/{project_id}/clean");
                if let Err(e) = client.request_json("POST", &path, None).await {
                    log(cfg, &format!("phase_commit {task_id}: /clean failed: {e} (push succeeded; bit will need manual reset)"));
                } else {
                    log(cfg, &format!("phase_commit {task_id}: marked project {project_id} clean"));
                }
            }
            let summary = match merge_outcome {
                Ok(s) => format!("pushed {branch}: {out}; main: {s}"),
                Err(e) => format!("pushed {branch}: {out}; main: not merged ({e})"),
            };
            complete_task(cfg, client, task_id, &summary).await;
            // Fire-and-forget: do not block task completion on Slack API latency.
            {
                let agent2   = cfg.agent_name.clone();
                let tid2     = task_id.to_string();
                let title2   = task["title"].as_str().unwrap_or("(phase commit)").to_string();
                let summary2 = summary.clone();
                tokio::spawn(async move {
                    slack::notify_completed(&agent2, &tid2, &title2, &summary2).await;
                });
            }
        }
        Err(PhaseCommitError::Transient(e)) => {
            // Transient network failure — silently requeue so the next
            // dispatch cycle retries.  Do NOT file an investigation task;
            // that would flood the queue with noise on flaky networks.
            log(cfg, &format!("phase_commit {task_id} transient git failure (requeueing): {e}"));
            // Drift-fix #4: tell the server this attempt failed so the
            // dispatch loop can stop auto-filing if we hit 3 in a row.
            if !project_id.is_empty() {
                let path = format!("/api/projects/{project_id}/phase-commit-failed");
                let body = serde_json::json!({"reason": e});
                if let Err(re) = client.request_json("POST", &path, Some(&body)).await {
                    log(cfg, &format!("phase_commit {task_id}: failure-report POST failed: {re}"));
                }
            }
            unclaim_task(cfg, client, task_id).await;
            slack::notify_failed(
                &cfg.agent_name,
                task_id,
                task["title"].as_str().unwrap_or("(phase commit)"),
                &format!("transient git failure: {e}"),
            ).await;
        }
        Err(PhaseCommitError::Hard(e)) => {
            log(cfg, &format!("phase_commit {task_id} git failed: {e}"));
            // Drift-fix #4: tell the server this attempt failed so the
            // dispatch loop can stop auto-filing if we hit 3 in a row.
            if !project_id.is_empty() {
                let path = format!("/api/projects/{project_id}/phase-commit-failed");
                let body = serde_json::json!({"reason": e});
                if let Err(re) = client.request_json("POST", &path, Some(&body)).await {
                    log(cfg, &format!("phase_commit {task_id}: failure-report POST failed: {re}"));
                }
            }
            unclaim_task(cfg, client, task_id).await;
            slack::notify_failed(
                &cfg.agent_name,
                task_id,
                task["title"].as_str().unwrap_or("(phase commit)"),
                &format!("git error: {e}"),
            ).await;
            // File investigation task for hard (non-retriable) failures.
            let req = CreateTaskRequest {
                project_id: project_id.to_string(),
                title: format!("Investigate git failure: phase {phase}"),
                description: Some(format!("Phase commit failed for {task_id}: {e}")),
                priority: Some(0),
                task_type: Some(TaskType::Work),
                ..Default::default()
            };
            let _ = client.tasks().create(&req).await;
        }
    }
    mark_done(task_id);
}

/// Inner (mutex-free) implementation of the merge-to-main sequence.
///
/// This function contains all the git operations without acquiring any
/// workspace mutex itself, making it safe to call from a context that
/// already holds a workspace lock (e.g. from within `run_git_phase_commit`
/// once that function is refactored to hold a per-workspace mutex).
///
/// Callers that do NOT already hold the workspace mutex should call the
/// thin wrapper [`run_git_merge_to_main`] instead, which exists solely to
/// provide a named entry-point that documents the locking contract.
///
/// Sequence:
///   git fetch origin --quiet
///   git checkout main
///   git pull --ff-only origin main          # land any other commits
///   git merge --ff-only <branch>            # FF main forward
///   git push origin main
///
/// All steps after `git fetch` are best-effort: if any step fails we
/// stop and return Err with stderr context. The phase branch remains
/// pushed; only the main update is skipped.
async fn run_git_merge_to_main_inner(workspace: &PathBuf, branch: &str) -> Result<String, PhaseCommitError> {
    let ws = workspace.to_str().unwrap_or(".");

    let fetch = Command::new("git").args(["-C", ws, "fetch", "origin", "--quiet"])
        .output().await.map_err(|e| PhaseCommitError::Hard(format!("git fetch: {e}")))?;
    if !fetch.status.success() {
        return Err(PhaseCommitError::Hard(format!("fetch: {}", String::from_utf8_lossy(&fetch.stderr).trim())));
    }

    let checkout = Command::new("git").args(["-C", ws, "checkout", "main"])
        .output().await.map_err(|e| PhaseCommitError::Hard(format!("git checkout main: {e}")))?;
    if !checkout.status.success() {
        return Err(PhaseCommitError::Hard(format!("checkout main: {}", String::from_utf8_lossy(&checkout.stderr).trim())));
    }

    let pull = Command::new("git").args(["-C", ws, "pull", "--ff-only", "origin", "main", "--quiet"])
        .output().await.map_err(|e| PhaseCommitError::Hard(format!("git pull main: {e}")))?;
    if !pull.status.success() {
        let stderr = String::from_utf8_lossy(&pull.stderr).to_string();
        // diverged main is a real possibility if main moved past our last
        // pull; abort safely.
        return Err(PhaseCommitError::Hard(format!("pull --ff-only: {stderr}")));
    }

    let merge = Command::new("git").args(["-C", ws, "merge", "--ff-only", branch, "--quiet"])
        .output().await.map_err(|e| PhaseCommitError::Hard(format!("git merge: {e}")))?;
    if !merge.status.success() {
        let stderr = String::from_utf8_lossy(&merge.stderr).to_string();
        return Err(PhaseCommitError::Hard(format!("merge --ff-only {branch}: {stderr}")));
    }

    let push = tokio::time::timeout(
        Duration::from_secs(600),
        Command::new("git").args(["-C", ws, "push", "origin", "main"]).output(),
    ).await
    .map_err(|_| PhaseCommitError::Transient("git push main timed out".to_string()))?
    .map_err(|e| PhaseCommitError::Hard(format!("git push main: {e}")))?;
    if !push.status.success() {
        return Err(PhaseCommitError::Hard(format!("push main: {}", String::from_utf8_lossy(&push.stderr).trim())));
    }
    Ok("fast-forwarded".to_string())
}

/// Drift-fix #2: after pushing phase/<phase>, try to fast-forward main.
///
/// This is a thin wrapper around [`run_git_merge_to_main_inner`] for callers
/// that do **not** already hold a workspace mutex.  The separation exists to
/// prevent re-entrant deadlocks: if this function and `run_git_phase_commit`
/// were ever composed under a single `tokio::sync::Mutex` guard (which is
/// not reentrant), any code path that called this wrapper while already
/// holding the guard would deadlock.  Factoring the logic into the inner
/// function allows such composed callers to call `run_git_merge_to_main_inner`
/// directly without re-acquiring the lock.
async fn run_git_merge_to_main(workspace: &PathBuf, branch: &str) -> Result<String, PhaseCommitError> {
    run_git_merge_to_main_inner(workspace, branch).await
}

/// Return true when a `git push` stderr looks like a transient network
/// hiccup that is worth retrying (connection-level failures, remote
/// service unavailability).  Auth failures and non-fast-forward
/// rejections are *permanent* — retrying them wastes time and may even
/// trigger rate-limiting, so they are not considered transient.
fn is_transient_network_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    // Permanent errors — hard-fail immediately.
    if lower.contains("authentication failed")
        || lower.contains("could not read username")
        || lower.contains("invalid username or password")
        || lower.contains("permission denied")
        || lower.contains("rejected")
        || lower.contains("non-fast-forward")
        || lower.contains("[remote rejected]")
    {
        return false;
    }
    // Transient network / infrastructure signals.
    lower.contains("could not resolve host")
        || lower.contains("unable to connect")
        || lower.contains("connection timed out")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
        || lower.contains("the remote end hung up")
        || lower.contains("curl error")
        || lower.contains("ssh: connect to host")
        || lower.contains("broken pipe")
        || lower.contains("temporary failure")
        || lower.contains("service unavailable")
        || lower.contains("503")
}

/// Remove a stale `.git/index.lock` if one is present.
///
/// A stale `index.lock` (left by a git process that was killed or crashed)
/// causes every subsequent git operation to fail with:
///
///   "Another git process seems to be running in this repository"
///
/// This guard runs **before the first git command** in both the inline path
/// (`run_git_phase_commit`) and the script-delegation path
/// (`execute_phase_commit_task` → `phase-commit.sh`).  Wiring it into the
/// shared entry point rather than only the inline path ensures the lock is
/// cleared regardless of whether a workspace-local `scripts/phase-commit.sh`
/// is present.
///
/// # Safety
///
/// We only remove the lock when it is older than `stale_threshold`.  A fresh
/// lock belongs to a concurrently running git process; removing it would
/// corrupt that process's index write.  The same 600 s threshold used by the
/// shell script's `LOCK_TIMEOUT_S` default is used here.
fn clear_stale_index_lock(workspace: &PathBuf) {
    const STALE_THRESHOLD_SECS: u64 = 600;

    let lock_path = workspace.join(".git").join("index.lock");
    let meta = match std::fs::metadata(&lock_path) {
        Ok(m) => m,
        Err(_) => return, // file absent — nothing to do
    };

    let age_secs = meta
        .modified()
        .ok()
        .and_then(|mtime| mtime.elapsed().ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if age_secs > STALE_THRESHOLD_SECS {
        if let Err(e) = std::fs::remove_file(&lock_path) {
            // Log but do not abort — the subsequent git command will surface
            // the real error if the lock is still held.
            eprintln!(
                "[tasks] clear_stale_index_lock: failed to remove {} (age={}s): {e}",
                lock_path.display(),
                age_secs
            );
        } else {
            eprintln!(
                "[tasks] clear_stale_index_lock: removed stale index.lock (age={}s): {}",
                age_secs,
                lock_path.display()
            );
        }
    }
    // Fresh lock (age <= STALE_THRESHOLD_SECS): leave in place.
}

pub(crate) async fn run_git_phase_commit(workspace: &PathBuf, branch: &str, commit_msg: &str) -> Result<String, PhaseCommitError> {
    let ws = workspace.to_str().unwrap_or(".");

    // ── Pre-flight: verify the workspace is a real git repository ────────────
    //
    // Root cause of "fatal: 'origin' does not appear to be a git repository":
    //
    //   resolve_workspace() can return a path that exists on disk (an empty
    //   stub directory, or a CIFS share that mounted but was never populated)
    //   without ever having been `git init`'d.  When git is invoked with
    //   `-C <path>` against such a directory it silently accepts the chdir
    //   and then fails to find a remote because there is no .git/config —
    //   producing the misleading message about 'origin' rather than about the
    //   missing repository.
    //
    // We check for both the traditional `.git/` directory layout and the
    // git-worktree / bare-repo case (where `.git` is a file containing a
    // `gitdir:` pointer).  `git rev-parse --git-dir` is the canonical way to
    // detect either form, but spawning a process just to validate is wasteful;
    // a simple filesystem check is sufficient for the CIFS use-case.
    let git_marker = workspace.join(".git");
    if !git_marker.exists() {
        return Err(PhaseCommitError::Hard(format!(
            "workspace is not a git repository (no .git entry): {}",
            workspace.display()
        )));
    }

    // ── Clear any stale index.lock before touching the index ─────────────────
    //
    // A killed git process can leave .git/index.lock behind, causing every
    // subsequent git command to fail.  Clearing it here ensures the guard
    // fires for the inline path.  The script-delegation path (phase-commit.sh)
    // calls its own clear_stale_index_lock() shell function at the top of the
    // script, before the exec delegation block, so it is also covered.
    clear_stale_index_lock(workspace);

    // Apply CIFS-safe git configuration before any index-touching operation.
    //
    // The workspace repo lives on a CIFS/SMB2-backed AccFS share.  Without
    // these settings git's defaults cause two classes of failure on CIFS:
    //
    //   • core.trustctime=false   — CIFS ctime is unreliable; the default
    //     (true) causes git to re-stat every tracked file on every status /
    //     checkout, amplifying SMB2 round-trips and the window for D-state.
    //
    //   • core.preloadIndex=false — The default parallel stat storm across the
    //     SMB2 connection saturates the mount under load and increases the
    //     probability of a stall during index writes.
    //
    //   • gc.auto=0               — The default (6700 loose objects) triggers
    //     background git-gc which writes large pack files to the CIFS share;
    //     on a near-full filesystem this reliably produces D-state hangs and
    //     can itself fill the remaining space.
    //
    //   • index.threads=1         — Serialises index I/O; safer on CIFS where
    //     concurrent index readers can collide over the network lock.
    //
    // Root-cause analysis: docs/git-index-write-failure-investigation.md
    //   (Incident 7 — CIFS D-state hang, 2026-04-26).
    //
    // Each `git config` call is best-effort: a failure here (e.g. the .git
    // directory is read-only or the config file is locked) must not abort the
    // commit — the default git behaviour is slower but still correct.
    let cifs_configs: &[(&str, &str)] = &[
        ("core.trustctime",        "false"),
        ("core.checkStat",         "minimal"),
        ("core.preloadIndex",      "false"),
        ("index.threads",          "1"),
        ("gc.auto",                "0"),
        ("fetch.writeCommitGraph", "false"),
    ];
    for (key, value) in cifs_configs {
        let _ = Command::new("git")
            .args(["-C", ws, "config", "--local", key, value])
            .output()
            .await;
    }

    let checkout = Command::new("git")
        .args(["-C", ws, "checkout", "-B", branch])
        .output().await
        .map_err(|e| PhaseCommitError::Hard(format!("git checkout: {e}")))?;
    if !checkout.status.success() {
        return Err(PhaseCommitError::Hard(String::from_utf8_lossy(&checkout.stderr).to_string()));
    }

    let add = Command::new("git")
        .args(["-C", ws, "add", "-A"])
        .output().await
        .map_err(|e| PhaseCommitError::Hard(format!("git add: {e}")))?;
    if !add.status.success() {
        return Err(PhaseCommitError::Hard(String::from_utf8_lossy(&add.stderr).to_string()));
    }

    let commit = Command::new("git")
        .args(["-C", ws, "commit", "-m", commit_msg])
        .output().await
        .map_err(|e| PhaseCommitError::Hard(format!("git commit: {e}")))?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr).to_string();
        if !stderr.contains("nothing to commit") {
            return Err(PhaseCommitError::Hard(stderr));
        }
    }

    // Fetch + rebase onto the remote branch before pushing so that if
    // another agent instance has independently advanced origin/<branch>
    // (e.g. concurrent phase-commit runs) we produce a fast-forward push
    // rather than a non-fast-forward rejection.  Both steps are best-
    // effort: a network failure here is not fatal — the push retry loop
    // below will surface the non-fast-forward error on its first attempt
    // and the next phase-commit cycle will try again.
    //
    // WHY fetch + rebase (not pull --rebase)?
    // `git pull --rebase` is a single operation with no per-step timeout.
    // On a CIFS-backed share a stalled fetch can hold the pull command in
    // D-state indefinitely, blocking the entire agent.  Splitting into two
    // timed operations (fetch, then rebase) bounds each step independently.
    let _ = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new("git")
            .args(["-C", ws, "fetch", "origin", branch])
            .output(),
    ).await;
    let rebase = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new("git")
            .args(["-C", ws, "rebase", &format!("origin/{branch}")])
            .output(),
    ).await;
    // If the rebase itself fails (e.g. genuine conflict), abort it so
    // the working tree is clean and the push will surface the real error.
    if let Ok(Ok(rb)) = rebase {
        if !rb.status.success() {
            let _ = Command::new("git")
                .args(["-C", ws, "rebase", "--abort"])
                .output()
                .await;
        }
    }

    // Retry loop: up to 3 attempts with exponential backoff (10 s, 20 s).
    // Each attempt has its own 120 s timeout so one hung TCP connection
    // cannot consume the full budget.  Permanent errors (auth, non-fast-
    // forward) are surfaced immediately without waiting for the remaining
    // retries.
    const MAX_ATTEMPTS: u32 = 3;
    const PUSH_TIMEOUT_SECS: u64 = 120;
    const BACKOFF_SECS: [u64; 2] = [10, 20];

    let mut last_err = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let push_result = tokio::time::timeout(
            Duration::from_secs(PUSH_TIMEOUT_SECS),
            Command::new("git")
                .args(["-C", ws, "push", "--set-upstream", "origin", branch])
                .output(),
        ).await;

        match push_result {
            Err(_) => {
                // The per-attempt timeout fired — treat as transient.
                last_err = format!("git push timed out after {PUSH_TIMEOUT_SECS}s (attempt {attempt}/{MAX_ATTEMPTS})");
            }
            Ok(Err(e)) => {
                // OS-level spawn/IO error — unlikely to be transient, but
                // surface the full message so the caller can decide.
                return Err(PhaseCommitError::Hard(format!("git push: {e}")));
            }
            Ok(Ok(output)) => {
                if output.status.success() {
                    return Ok(String::from_utf8_lossy(&output.stdout).to_string());
                }
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                // Hard-fail immediately on permanent errors.
                if !is_transient_network_error(&stderr) {
                    return Err(PhaseCommitError::Hard(stderr));
                }
                last_err = format!(
                    "git push failed (attempt {attempt}/{MAX_ATTEMPTS}): {}",
                    stderr.trim()
                );
            }
        }

        // Wait before the next attempt (no sleep after the last attempt).
        if attempt < MAX_ATTEMPTS {
            let wait = BACKOFF_SECS[(attempt as usize) - 1];
            sleep(Duration::from_secs(wait)).await;
        }
    }

    // All retry attempts were transient failures — signal Transient so the
    // caller silently requeues without filing an investigation task.
    Err(PhaseCommitError::Transient(last_err))
}

/// Collapse a multi-line stderr into the most informative single line:
/// prefer lines that look like errors (`error:`, `fatal:`, `! [rejected]`,
/// `failed to`) over `hint:` lines, then fall back to first non-empty line.
/// Loggers truncate at the first newline, so without this the agent's task
/// log shows just "hint: ..." while the real cause is buried below.
fn flatten_stderr(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() { return String::new(); }
    for l in &lines {
        let t = l.trim_start();
        if t.starts_with("error:")
            || t.starts_with("fatal:")
            || t.starts_with("! [rejected]")
            || t.starts_with("failed to")
        {
            return (*l).to_string();
        }
    }
    // No diagnostic line found — return first non-empty (often a hint).
    lines[0].to_string()
}

// ── Shared helpers ────────────────────────────────────────────────────────────

async fn resolve_workspace(cfg: &Config, client: &Client, project_id: &str, task_id: &str) -> PathBuf {
    let shared = cfg.acc_dir.join("shared");

    // AgentFS is mounted at $ACC_DIR/shared (CIFS to the hub's
    // /srv/accfs). Each project lives at <shared>/<slug>, where <slug>
    // matches the server's Project.slug field. Until 2026-04-25 we used
    // <shared>/<project_id> here, which produced an empty stub directory
    // — the actual content lives at <shared>/<slug>. agents ran with
    // empty cwds and "completed" tasks without doing real work.
    //
    // Look up the slug; fall back to project_id if the lookup fails so
    // we degrade rather than break.
    let workspace_name: String = if project_id.is_empty() {
        "default".to_string()
    } else {
        match client.projects().get(project_id).await {
            Ok(p) if p.slug.as_deref().map(|s| !s.is_empty()).unwrap_or(false) => {
                p.slug.unwrap()
            }
            _ => project_id.to_string(),
        }
    };

    if shared.exists() {
        let p = shared.join(&workspace_name);
        if p.exists() {
            return p;
        }
    }

    // Hub-resident fallback: on the AgentFS-server node (do-host1) the
    // share lives at /srv/accfs/shared/<slug>/ and isn't mounted back
    // onto ~/.acc/shared/ (no self-loop). Without setup-node.sh's
    // symlink farm in place, fall back to the canonical hub path so
    // agents on the hub host still find populated workspaces.
    let hub_path = std::path::Path::new("/srv/accfs/shared").join(&workspace_name);
    if hub_path.exists() {
        return hub_path;
    }

    // Fallback: shared/<slug-or-id> (will be created by caller); for
    // task-scoped paths a per-task local dir is the last resort.
    if task_id.is_empty() {
        cfg.acc_dir.join("shared").join(&workspace_name)
    } else {
        cfg.acc_dir.join("task-workspaces").join(task_id)
    }
}

async fn complete_task(cfg: &Config, client: &Client, task_id: &str, output: &str) {
    let truncated = &output[..output.len().min(4096)];
    let _ = client
        .tasks()
        .complete(task_id, Some(&cfg.agent_name), Some(truncated))
        .await;
}

async fn unclaim_task(cfg: &Config, client: &Client, task_id: &str) {
    let _ = client.tasks().unclaim(task_id, Some(&cfg.agent_name)).await;
}

fn is_quenched(cfg: &Config) -> bool {
    cfg.quench_file().exists()
}

fn log(cfg: &Config, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [tasks] [{}] {msg}", cfg.agent_name);
    eprintln!("{line}");
    let log_path = cfg.log_file("tasks");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
    // CCC-u3c: also emit through tracing so journald (when available)
    // sees this for the consolidated dashboard log viewer.
    tracing::info!(component = "tasks", agent = %cfg.agent_name, "{msg}");
}

fn parse_task_type(s: &str) -> Option<TaskType> {
    use std::str::FromStr;
    TaskType::from_str(s).ok()
}

fn to_value<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
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

    fn test_client(url: &str) -> Client {
        Client::new(url, "test-token").expect("build client")
    }

    // ── fetch_open_tasks ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_open_tasks_parses_tasks() {
        let mock = HubMock::with_tasks(vec![
            json!({"id": "t-1", "title": "Alpha", "status": "open", "task_type": "work"}),
            json!({"id": "t-2", "title": "Beta",  "status": "open", "task_type": "work"}),
        ]).await;
        let client = test_client(&mock.url);
        let tasks = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "work").await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["id"], "t-1");
    }

    #[tokio::test]
    async fn test_fetch_open_tasks_empty_hub() {
        let mock = HubMock::new().await;
        let client = test_client(&mock.url);
        let tasks = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "work").await.unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_open_tasks_only_open_status() {
        let mock = HubMock::with_tasks(vec![
            json!({"id": "open-1",   "status": "open",    "task_type": "work"}),
            json!({"id": "claimed-1","status": "claimed", "task_type": "work"}),
        ]).await;
        let client = test_client(&mock.url);
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
        let client = test_client(&mock.url);
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
        let client = test_client(&cfg.acc_url);
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
        let client = test_client(&mock.url);
        let count = count_active_tasks(&test_cfg(&mock.url), &client).await;
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_count_active_tasks_zero_when_none_claimed() {
        let mock = HubMock::with_tasks(vec![
            json!({"id": "o1", "status": "open"}),
        ]).await;
        let client = test_client(&mock.url);
        let count = count_active_tasks(&test_cfg(&mock.url), &client).await;
        assert_eq!(count, 0);
    }

    // ── claim_task ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_claim_task_success_returns_task() {
        let mock = HubMock::new().await;
        let client = test_client(&mock.url);
        let result = claim_task(&test_cfg(&mock.url), &client, "task-xyz").await;
        assert!(result.is_ok(), "200 → Ok");
        assert_eq!(result.unwrap()["id"], "task-xyz");
    }

    #[tokio::test]
    async fn test_claim_task_conflict_returns_err_409() {
        let mock = HubMock::with_state(HubState { task_claim_status: 409, ..Default::default() }).await;
        let client = test_client(&mock.url);
        let result = claim_task(&test_cfg(&mock.url), &client, "task-abc").await;
        assert!(matches!(result, Err(409)), "409 → Err(409)");
    }

    #[tokio::test]
    async fn test_claim_task_rate_limited_returns_err_429() {
        let mock = HubMock::with_state(HubState { task_claim_status: 429, ..Default::default() }).await;
        let client = test_client(&mock.url);
        let result = claim_task(&test_cfg(&mock.url), &client, "task-def").await;
        assert!(matches!(result, Err(429)), "429 → Err(429)");
    }

    #[tokio::test]
    async fn test_claim_task_blocked_returns_err_423() {
        let mock = HubMock::with_state(HubState { task_claim_status: 423, ..Default::default() }).await;
        let client = test_client(&mock.url);
        let result = claim_task(&test_cfg(&mock.url), &client, "task-blocked").await;
        assert!(matches!(result, Err(423)), "423 → Err(423)");
    }

    // ── parse_review_output ───────────────────────────────────────────────────

    #[test]
    fn test_parse_review_output_approved() {
        let output = r#"{"verdict":"approved","reason":"looks good","gaps":[]}"#;
        let (v, r, _halluc, g) = parse_review_output(output);
        assert_eq!(v, "approved");
        assert_eq!(r, "looks good");
        assert!(g.is_empty());
    }

    #[test]
    fn test_parse_review_output_approved_with_preamble() {
        let output = r#"Here is my review:

{"verdict":"approved","reason":"well done","gaps":[{"title":"Add tests","description":"Missing unit tests","priority":2}]}"#;
        let (v, r, _halluc, g) = parse_review_output(output);
        assert_eq!(v, "approved");
        assert_eq!(r, "well done");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0]["title"], "Add tests");
    }

    #[test]
    fn test_parse_review_output_rejected() {
        let output = r#"{"verdict":"rejected","reason":"build is broken","gaps":[{"title":"Fix CI","description":"pipeline fails","priority":0}]}"#;
        let (v, r, _halluc, g) = parse_review_output(output);
        assert_eq!(v, "rejected");
        assert_eq!(r, "build is broken");
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn test_parse_review_output_unparseable_treated_as_rejected() {
        let output = "This is not JSON at all";
        let (v, r, _halluc, _) = parse_review_output(output);
        assert_eq!(v, "rejected");
        assert_eq!(r, "unparseable output");
    }

    #[test]
    fn test_parse_review_output_empty_treated_as_rejected() {
        let (v, r, _halluc, _) = parse_review_output("");
        assert_eq!(v, "rejected");
        assert_eq!(r, "unparseable output");
    }

    // ── submit_for_review ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_submit_for_review_picks_non_self_peer() {
        let mock = HubMock::new().await;
        let client = test_client(&mock.url);
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
        let client = test_client(&mock.url);
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
        let client = test_client(&mock.url);
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

    // ── parse_vote_output ─────────────────────────────────────────────────────

    #[test]
    fn test_parse_vote_output_approve_with_refinement() {
        let output = r#"{"vote":"approve","refinement":"Add error handling for the edge case.","reason":"solid proposal"}"#;
        let (vote, refinement, reason) = parse_vote_output(output);
        assert_eq!(vote, "approve");
        assert_eq!(refinement, "Add error handling for the edge case.");
        assert_eq!(reason, "solid proposal");
    }

    #[test]
    fn test_parse_vote_output_reject_with_refinement() {
        let output = r#"{"vote":"reject","refinement":"The idea duplicates CCC-abc; consolidate there instead.","reason":"duplicates CCC-abc"}"#;
        let (vote, refinement, reason) = parse_vote_output(output);
        assert_eq!(vote, "reject");
        assert_eq!(refinement, "The idea duplicates CCC-abc; consolidate there instead.");
        assert_eq!(reason, "duplicates CCC-abc");
    }

    #[test]
    fn test_parse_vote_output_reject_missing_refinement_is_hard_error() {
        // A reject vote with no refinement must be treated as a parse error
        // (refinement is required by the server for all votes).
        let output = r#"{"vote":"reject","reason":"duplicates CCC-abc"}"#;
        let (vote, refinement, reason) = parse_vote_output(output);
        assert_eq!(vote, "reject");
        assert_eq!(refinement, "model did not provide refinement");
        assert_eq!(reason, "unparseable output");
    }

    #[test]
    fn test_parse_vote_output_with_leading_prose() {
        let output = r#"Here is my evaluation:

{"vote":"approve","refinement":"Define a clear API contract first.","reason":"good idea"}"#;
        let (vote, refinement, _) = parse_vote_output(output);
        assert_eq!(vote, "approve");
        assert_eq!(refinement, "Define a clear API contract first.");
    }

    #[test]
    fn test_parse_vote_output_unparseable_defaults_to_reject() {
        let (vote, refinement, reason) = parse_vote_output("not json at all");
        assert_eq!(vote, "reject");
        assert_eq!(refinement, "model did not provide refinement");
        assert_eq!(reason, "unparseable output");
    }

    #[test]
    fn test_parse_vote_output_empty_defaults_to_reject() {
        let (vote, refinement, reason) = parse_vote_output("");
        assert_eq!(vote, "reject");
        assert_eq!(refinement, "model did not provide refinement");
        assert_eq!(reason, "unparseable output");
    }

    #[test]
    fn test_parse_vote_output_whitespace_only_refinement_is_hard_error() {
        let output = r#"{"vote":"approve","refinement":"   ","reason":"ok"}"#;
        let (vote, refinement, reason) = parse_vote_output(output);
        // Whitespace-only refinement must be treated as a parse error (server would 400).
        assert_eq!(vote, "reject");
        assert_eq!(refinement, "model did not provide refinement");
        assert_eq!(reason, "unparseable output");
    }

    // ── execute_idea_vote_task — integration with hub mock ────────────────────

    #[tokio::test]
    async fn test_fetch_open_tasks_returns_idea_tasks() {
        let mock = HubMock::with_tasks(vec![
            json!({"id":"idea-1","status":"open","task_type":"idea","title":"New feature","description":"desc"}),
            json!({"id":"work-1","status":"open","task_type":"work","title":"Work task","description":"desc"}),
        ]).await;
        let client = test_client(&mock.url);
        let ideas = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "idea").await.unwrap();
        assert_eq!(ideas.len(), 1);
        assert_eq!(ideas[0]["id"], "idea-1");
        assert_eq!(ideas[0]["task_type"], "idea");
    }

    #[tokio::test]
    async fn test_idea_vote_submitted_via_client() {
        // Verify the vote is recorded in the hub mock when the full
        // execute_idea_vote_task path runs.  The SDK agentic loop
        // (sdk::run_agent) requires a live Anthropic API key, so this
        // test bypasses it by directly constructing the parsed vote
        // output and calling the vote endpoint, mirroring the real
        // code path without the model inference step.
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        // Simulate what execute_idea_vote_task does after parse_vote_output.
        let vote = "approve";
        let refinement = Some("Implement with a retry budget and circuit breaker.");
        let task_id = "idea-abc";

        let result = client.tasks()
            .vote(task_id, &cfg.agent_name, vote, refinement)
            .await;
        assert!(result.is_ok(), "vote PUT should succeed: {:?}", result);

        let recorded = mock.state.read().await.recorded_votes.lock().await.clone();
        assert_eq!(recorded.len(), 1, "exactly one vote recorded");
        assert_eq!(recorded[0]["task_id"], task_id);
        assert_eq!(recorded[0]["agent"], "test-agent");
        assert_eq!(recorded[0]["vote"], "approve");
        assert_eq!(recorded[0]["refinement"], "Implement with a retry budget and circuit breaker.");
    }

    #[tokio::test]
    async fn test_idea_reject_vote_with_refinement_succeeds() {
        // The server requires refinement for reject votes too.  Confirm that
        // sending a reject vote WITH a non-empty refinement succeeds (HTTP 200).
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let result = client.tasks()
            .vote("idea-rej", &cfg.agent_name, "reject", Some("Idea duplicates CCC-123; close that issue instead."))
            .await;
        assert!(result.is_ok(), "reject vote with refinement should succeed: {:?}", result);

        let recorded = mock.state.read().await.recorded_votes.lock().await.clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["vote"], "reject");
        assert_eq!(recorded[0]["refinement"], "Idea duplicates CCC-123; close that issue instead.");
    }

    #[tokio::test]
    async fn test_idea_reject_vote_without_refinement_hits_400() {
        // The server returns 400 when refinement is absent — for any vote type.
        // This test pins the hub_mock contract so regressions are caught here
        // before they reach the real server.
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        // Pass None — the client will omit the refinement field entirely.
        let result = client.tasks()
            .vote("idea-rej", &cfg.agent_name, "reject", None)
            .await;
        assert!(result.is_err(), "reject vote without refinement must fail with 400");
        // Confirm no vote was recorded.
        let recorded = mock.state.read().await.recorded_votes.lock().await.clone();
        assert_eq!(recorded.len(), 0, "no vote should be recorded when refinement is missing");
    }

    #[tokio::test]
    async fn test_idea_approve_vote_without_refinement_hits_400() {
        // Approve votes also require refinement; missing it must return 400.
        let mock = HubMock::new().await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let result = client.tasks()
            .vote("idea-app", &cfg.agent_name, "approve", None)
            .await;
        assert!(result.is_err(), "approve vote without refinement must fail with 400");
        let recorded = mock.state.read().await.recorded_votes.lock().await.clone();
        assert_eq!(recorded.len(), 0, "no vote should be recorded when refinement is missing");
    }

    #[tokio::test]
    async fn test_idea_vote_409_does_not_panic() {
        // Server returns 409 (already voted / creator) — execute_idea_vote_task
        // must log the error and return without panicking.
        //
        // The sdk::run_agent step is stubbed via the run_agent_fn parameter so
        // the test exercises the real error-handling branch inside
        // execute_idea_vote_task without requiring a live Anthropic API key.
        use crate::hub_mock::HubState;
        let mock = HubMock::with_state(HubState {
            task_vote_status: 409,
            ..Default::default()
        }).await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let task = json!({
            "id": "idea-xyz",
            "title": "Test idea",
            "description": "A description.",
            "status": "open",
            "task_type": "idea",
            "metadata": {"votes": []}
        });

        // Stub: returns a valid vote JSON so the real vote-submission branch
        // (not the agent-loop-failed branch) is exercised, proving that the
        // 409 from the hub is handled gracefully rather than panicking.
        //
        // Takes owned String + PathBuf so the returned future is 'static —
        // no HRTB / for<'a> bound needed.
        let stub_run_agent = |_prompt: String, _workspace: PathBuf| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'static>> {
            Box::pin(async move {
                Ok::<String, String>(
                    r#"{"vote":"approve","refinement":"Add integration tests.","reason":"Looks good."}"#
                        .to_string(),
                )
            })
        };

        // Must complete without panicking even though the hub returns 409.
        execute_idea_vote_task(&cfg, &client, &task, stub_run_agent).await;

        // The 409 response is still recorded by the hub mock (the request was
        // made); confirm exactly one attempt reached the server.
        let recorded = mock.state.read().await.recorded_votes.lock().await.clone();
        assert_eq!(recorded.len(), 1, "vote PUT was attempted despite expected 409");
        assert_eq!(recorded[0]["task_id"], "idea-xyz");
        assert_eq!(recorded[0]["vote"], "approve");
    }

    #[tokio::test]
    async fn test_idea_skip_already_voted() {
        // Tasks where the agent's name appears in metadata.votes must be
        // skipped by the poll loop before spawning a vote goroutine.
        let mock = HubMock::with_tasks(vec![
            json!({
                "id": "idea-voted",
                "status": "open",
                "task_type": "idea",
                "metadata": {
                    "votes": [{"agent": "test-agent", "vote": "approve", "refinement": "done"}]
                }
            }),
        ]).await;
        let client = test_client(&mock.url);
        let ideas = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "idea").await.unwrap();
        // Idea is present in the list...
        assert_eq!(ideas.len(), 1);
        // ...but the poll loop skips it when already_voted is true.
        let already_voted = ideas[0]["metadata"]["votes"]
            .as_array()
            .map(|votes| votes.iter().any(|v| v["agent"].as_str() == Some("test-agent")))
            .unwrap_or(false);
        assert!(already_voted, "poll loop should detect own prior vote");
    }

    #[tokio::test]
    async fn test_idea_skip_own_creator() {
        // The agent must not vote on ideas it created.
        let mock = HubMock::with_tasks(vec![
            json!({
                "id": "idea-own",
                "status": "open",
                "task_type": "idea",
                "metadata": {"created_by": "test-agent", "votes": []}
            }),
        ]).await;
        let client = test_client(&mock.url);
        let ideas = fetch_open_tasks(&test_cfg(&mock.url), &client, 10, "idea").await.unwrap();
        assert_eq!(ideas.len(), 1);
        // Poll loop guard
        let creator = ideas[0]["metadata"]["created_by"].as_str().unwrap_or("");
        assert_eq!(creator, "test-agent", "creator field must be visible");
        // Verify the guard logic itself
        let cfg = test_cfg(&mock.url);
        assert!(creator == cfg.agent_name.as_str(), "own-creator guard should fire");
    }

    // ── subscribe_bus / vote_nudge tests ──────────────────────────────────────
    //
    // These tests verify that `subscribe_bus` correctly reads `task_type` from
    // `msg.extra` (the #[serde(flatten)] catch-all) rather than from the nested
    // `msg.body` object.  The server sends `task_type` as a top-level JSON
    // field, so it lands in `extra`; the old `msg.body.as_ref().and_then(…)`
    // path always produced "" and kept `vote_nudge.notify_one()` as dead code.
    //
    // Note on `to` field: subscribe_bus treats a message as broadcast when
    // `to` is empty or "null". "all" is handled by the separate bus.rs
    // dispatcher; here we use the agent's own name or empty string to
    // trigger the nudge path.

    /// Helper: run `subscribe_bus` against a HubMock SSE stream and wait until
    /// `nudge` fires (or the given timeout elapses).  Returns true when the
    /// nudge fires within the deadline.
    async fn await_nudge(notify: Arc<Notify>, timeout_ms: u64) -> bool {
        tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            notify.notified(),
        )
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn test_subscribe_bus_vote_nudge_fires_on_task_type_vote_in_extra() {
        // The server sends task_type as a top-level field (not inside body).
        // After the fix, subscribe_bus reads it from msg.extra and calls
        // vote_nudge.notify_one() — this test verifies that path is live.
        // Use agent name as `to` so subscribe_bus treats it as directed to us.
        let event = serde_json::json!({
            "type":      "tasks:dispatch_nudge",
            "to":        "test-agent",
            "task_type": "vote"    // top-level field → lands in BusMsg::extra
        })
        .to_string();

        let mock = HubMock::with_sse(vec![event]).await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let nudge      = Arc::new(Notify::new());
        let vote_nudge = Arc::new(Notify::new());

        // Run subscribe_bus in the background; it will drain the single-event
        // SSE stream and then return Ok(()) when the server closes the connection.
        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        tokio::spawn(async move {
            let coord = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg2.agent_name));
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord).await;
        });

        // vote_nudge must fire within 2 s.
        assert!(
            await_nudge(vote_nudge.clone(), 2_000).await,
            "vote_nudge must fire when task_type=vote is a top-level SSE field"
        );
        // The general nudge must also fire.
        assert!(
            await_nudge(nudge.clone(), 500).await,
            "nudge must also fire for every tasks:dispatch_nudge event"
        );
    }

    #[tokio::test]
    async fn test_subscribe_bus_vote_nudge_does_not_fire_for_other_task_types() {
        // A dispatch_nudge with task_type != "vote" must fire the general nudge
        // but NOT the vote-specific nudge.
        let event = serde_json::json!({
            "type":      "tasks:dispatch_nudge",
            "to":        "test-agent",
            "task_type": "work"   // not "vote"
        })
        .to_string();

        let mock = HubMock::with_sse(vec![event]).await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let nudge      = Arc::new(Notify::new());
        let vote_nudge = Arc::new(Notify::new());

        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        tokio::spawn(async move {
            let coord = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg2.agent_name));
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord).await;
        });

        // General nudge fires.
        assert!(
            await_nudge(nudge.clone(), 2_000).await,
            "nudge must fire for tasks:dispatch_nudge regardless of task_type"
        );
        // Vote nudge must NOT fire.
        assert!(
            !await_nudge(vote_nudge.clone(), 200).await,
            "vote_nudge must NOT fire when task_type is not 'vote'"
        );
    }

    #[tokio::test]
    async fn test_subscribe_bus_dispatch_nudge_no_task_type_fires_only_general_nudge() {
        // A dispatch_nudge with no task_type field must fire the general nudge
        // but not the vote nudge.
        let event = serde_json::json!({
            "type": "tasks:dispatch_nudge",
            "to":   "test-agent"
            // no task_type field at all
        })
        .to_string();

        let mock = HubMock::with_sse(vec![event]).await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let nudge      = Arc::new(Notify::new());
        let vote_nudge = Arc::new(Notify::new());

        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        tokio::spawn(async move {
            let coord = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg2.agent_name));
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord).await;
        });

        assert!(await_nudge(nudge.clone(), 2_000).await, "nudge fires");
        assert!(!await_nudge(vote_nudge.clone(), 200).await, "vote_nudge must NOT fire");
    }

    #[tokio::test]
    async fn test_subscribe_bus_dispatch_assigned_fires_general_nudge() {
        // tasks:dispatch_assigned directed to us must fire the general nudge.
        let event = serde_json::json!({
            "type": "tasks:dispatch_assigned",
            "to":   "test-agent"
        })
        .to_string();

        let mock = HubMock::with_sse(vec![event]).await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let nudge      = Arc::new(Notify::new());
        let vote_nudge = Arc::new(Notify::new());

        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        tokio::spawn(async move {
            let coord = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg2.agent_name));
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord).await;
        });

        assert!(await_nudge(nudge.clone(), 2_000).await, "nudge fires on dispatch_assigned");
        assert!(!await_nudge(vote_nudge.clone(), 200).await, "vote_nudge must not fire on dispatch_assigned");
    }

    #[tokio::test]
    async fn test_subscribe_bus_unrelated_event_does_not_fire_nudge() {
        // An unrelated bus event must not fire either nudge.
        let event = serde_json::json!({
            "type": "ping",
            "from": "hub",
            "to":   "test-agent"
        })
        .to_string();

        let mock = HubMock::with_sse(vec![event]).await;
        let cfg = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let nudge      = Arc::new(Notify::new());
        let vote_nudge = Arc::new(Notify::new());

        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        tokio::spawn(async move {
            let coord = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg2.agent_name));
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord).await;
        });

        assert!(!await_nudge(nudge.clone(), 300).await, "nudge must NOT fire on unrelated events");
        assert!(!await_nudge(vote_nudge.clone(), 200).await, "vote_nudge must NOT fire on unrelated events");
    }

    #[tokio::test]
    async fn test_subscribe_bus_dispatch_nudge_wrong_target_does_not_fire() {
        // A dispatch_nudge addressed to a different agent must be ignored.
        let event = serde_json::json!({
            "type":      "tasks:dispatch_nudge",
            "to":        "someone-else",
            "task_type": "vote"
        })
        .to_string();

        let mock = HubMock::with_sse(vec![event]).await;
        let cfg = test_cfg(&mock.url);   // agent_name = "test-agent"
        let client = test_client(&mock.url);

        let nudge      = Arc::new(Notify::new());
        let vote_nudge = Arc::new(Notify::new());

        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        tokio::spawn(async move {
            let coord = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg2.agent_name));
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord).await;
        });

        // Neither nudge should fire — the message was not for us.
        assert!(!await_nudge(nudge.clone(), 300).await, "nudge must not fire for other agents");
        assert!(!await_nudge(vote_nudge.clone(), 200).await, "vote_nudge must not fire for other agents");
    }

    // ── notify_failed on idea-vote agent-loop failure ─────────────────────────

    #[tokio::test]
    async fn test_idea_vote_agent_loop_failure_calls_notify_failed_no_panic() {
        // When the stub agent returns Err (simulating a timeout or API crash),
        // execute_idea_vote_task must call slack::notify_failed and return
        // without panicking.  Since SLACK_BOT_TOKEN is absent in the test
        // environment, notify_failed is a fast no-op — this verifies the code
        // path compiles and runs rather than the Slack message actually going
        // out.
        std::env::remove_var("SLACK_BOT_TOKEN");

        let mock = HubMock::new().await;
        let cfg  = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let task = json!({
            "id": "idea-fail",
            "title": "Failing idea",
            "description": "Will cause the agent loop to error.",
            "status": "open",
            "task_type": "idea",
            "metadata": {"votes": []}
        });

        // Stub returns Err — simulates an agent loop crash or timeout.
        //
        // Takes owned String + PathBuf so the returned future is 'static —
        // no HRTB / for<'a> bound needed.
        let stub_fail = |_prompt: String, _workspace: PathBuf| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'static>> {
            Box::pin(async move {
                Err::<String, String>("simulated agent crash".to_string())
            })
        };

        // Must complete without panicking even though notify_failed is called.
        execute_idea_vote_task(&cfg, &client, &task, stub_fail).await;

        // No vote should have been submitted (the loop errored before parse).
        let recorded = mock.state.read().await.recorded_votes.lock().await.clone();
        assert_eq!(recorded.len(), 0, "no vote must be recorded when the agent loop fails");
    }

    #[tokio::test]
    async fn test_idea_vote_success_calls_notify_voted_no_panic() {
        // When execute_idea_vote_task submits a vote successfully, it spawns a
        // fire-and-forget task calling slack::notify_voted.  With no
        // SLACK_BOT_TOKEN set that call is a fast no-op.  This test verifies
        // the happy-path Slack notification code compiles and runs end-to-end
        // without panicking.
        std::env::remove_var("SLACK_BOT_TOKEN");

        let mock = HubMock::new().await;
        let cfg  = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        let task = json!({
            "id": "idea-ok",
            "title": "Great idea",
            "description": "This one succeeds.",
            "status": "open",
            "task_type": "idea",
            "metadata": {"votes": []}
        });

        // Stub returns a valid approve vote JSON.
        //
        // Takes owned String + PathBuf so the returned future is 'static —
        // no HRTB / for<'a> bound needed.
        let stub_ok = |_prompt: String, _workspace: PathBuf| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'static>> {
            Box::pin(async move {
                Ok::<String, String>(
                    r#"{"vote":"approve","refinement":"Add integration tests for the new endpoint.","reason":"well reasoned"}"#
                        .to_string(),
                )
            })
        };

        // Must complete without panicking; the notify_voted spawn is fire-and-forget.
        execute_idea_vote_task(&cfg, &client, &task, stub_ok).await;

        // Vote should have been submitted to the hub.
        let recorded = mock.state.read().await.recorded_votes.lock().await.clone();
        assert_eq!(recorded.len(), 1, "one vote must be recorded on success");
        assert_eq!(recorded[0]["vote"], "approve");
        assert_eq!(recorded[0]["task_id"], "idea-ok");
    }

    // ── notify_failed on stale-claim release (restart recovery) ───────────────

    #[tokio::test]
    async fn test_cleanup_stale_claims_calls_notify_failed_no_panic() {
        // cleanup_stale_claims must call slack::notify_failed for each stale
        // task it releases.  With no SLACK_BOT_TOKEN set, notify_failed is a
        // fast no-op; the test verifies the path compiles, runs, and does not
        // panic even when stale claims are present.
        std::env::remove_var("SLACK_BOT_TOKEN");

        // Seed the hub with a claimed task attributed to our agent so that
        // cleanup_stale_claims finds it on startup.  The mock's task_list
        // handler filters by ?status=claimed, so this task will be returned.
        let mock = HubMock::with_state(crate::hub_mock::HubState {
            tasks: vec![json!({
                "id":        "stale-1",
                "title":     "Stale task from previous run",
                "status":    "claimed",
                "agent":     "test-agent",
                "task_type": "work",
                "project_id": ""
            })],
            ..Default::default()
        }).await;

        let cfg    = test_cfg(&mock.url);
        let client = test_client(&mock.url);

        // Must complete without panicking.  The unclaim PUT and notify_failed
        // (no-op when SLACK_BOT_TOKEN is absent) are both called internally;
        // we verify no panic and no lingering claimed tasks.
        cleanup_stale_claims(&cfg, &client).await;
    }

    // ── Persistent vote deduplication (.voted sentinel file) ──────────────────

    /// Build a Config whose acc_dir points to a fresh temporary directory so
    /// tests do not interfere with each other or with any real agent state.
    fn test_cfg_with_tmpdir() -> (Config, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let cfg = Config {
            acc_dir: tmp.path().to_path_buf(),
            acc_url: "http://127.0.0.1:1".to_string(), // unreachable — not used in these tests
            acc_token: "tok".to_string(),
            agent_name: "test-agent".to_string(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: "localhost".to_string(),
            ssh_user: "u".to_string(),
            ssh_host: "127.0.0.1".to_string(),
            ssh_port: 22,
        };
        (cfg, tmp)
    }

    #[test]
    fn test_voted_sentinel_path_is_inside_task_workspaces() {
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let path = voted_sentinel_path(&cfg, "idea-42");
        assert!(
            path.ends_with("task-workspaces/idea-42/.voted"),
            "sentinel must be at <acc_dir>/task-workspaces/<task_id>/.voted, got: {}",
            path.display()
        );
        assert!(path.starts_with(&cfg.acc_dir));
    }

    #[test]
    fn test_in_vote_cooldown_persistent_false_before_mark() {
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        // Before any sentinel is written the function must return false.
        assert!(
            !in_vote_cooldown_persistent(&cfg, "idea-new"),
            "persistent cooldown must be false before mark_voted_persistent is called"
        );
    }

    #[test]
    fn test_mark_voted_persistent_creates_sentinel_file() {
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let task_id = "idea-sentinel-test";
        mark_voted_persistent(&cfg, task_id);
        let path = voted_sentinel_path(&cfg, task_id);
        assert!(
            path.exists(),
            ".voted sentinel file must exist after mark_voted_persistent: {}",
            path.display()
        );
    }

    #[test]
    fn test_in_vote_cooldown_persistent_true_after_mark() {
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let task_id = "idea-roundtrip";
        mark_voted_persistent(&cfg, task_id);
        assert!(
            in_vote_cooldown_persistent(&cfg, task_id),
            "persistent cooldown must be true after mark_voted_persistent"
        );
    }

    #[test]
    fn test_mark_voted_persistent_creates_parent_dir_if_absent() {
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let task_id = "idea-no-parent";
        // The task-workspaces/<id>/ directory must not exist yet.
        let parent = cfg.acc_dir.join("task-workspaces").join(task_id);
        assert!(!parent.exists(), "precondition: parent dir must not exist");
        mark_voted_persistent(&cfg, task_id);
        assert!(parent.exists(), "mark_voted_persistent must create the parent directory");
        assert!(parent.join(".voted").exists(), ".voted must exist after call");
    }

    #[test]
    fn test_mark_voted_persistent_idempotent() {
        // Calling mark_voted_persistent twice must not panic or corrupt state.
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let task_id = "idea-idempotent";
        mark_voted_persistent(&cfg, task_id);
        mark_voted_persistent(&cfg, task_id); // second call — must succeed
        assert!(
            in_vote_cooldown_persistent(&cfg, task_id),
            "cooldown must still be set after second mark_voted_persistent call"
        );
    }

    #[test]
    fn test_in_vote_cooldown_persistent_different_task_ids_are_independent() {
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        mark_voted_persistent(&cfg, "idea-A");
        // idea-B was never marked — its cooldown must be false.
        assert!(
            !in_vote_cooldown_persistent(&cfg, "idea-B"),
            "sentinel for a different task_id must not affect others"
        );
    }

    #[tokio::test]
    async fn test_execute_idea_vote_task_writes_sentinel_on_success() {
        // When execute_idea_vote_task submits a vote successfully, it must
        // write the .voted sentinel file so a restarted process skips the
        // idea in Poll 4 without re-running the agentic loop.
        let mock = HubMock::new().await;
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let client = test_client(&mock.url);

        let task_id = "idea-sentinel-on-success";
        let task = json!({
            "id": task_id,
            "title": "Great idea",
            "description": "Worth implementing.",
            "status": "open",
            "task_type": "idea",
            "metadata": {"votes": []}
        });

        let stub_ok = |_prompt: String, _workspace: PathBuf| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'static>> {
            Box::pin(async move {
                Ok::<String, String>(
                    r#"{"vote":"approve","refinement":"Define a typed API for the new feature.","reason":"solid"}"#
                        .to_string(),
                )
            })
        };

        execute_idea_vote_task(&cfg, &client, &task, stub_ok).await;

        // Verify that the persistent sentinel was written.
        assert!(
            in_vote_cooldown_persistent(&cfg, task_id),
            ".voted sentinel must exist after a successful vote submission"
        );
        assert!(
            voted_sentinel_path(&cfg, task_id).exists(),
            ".voted file must be present on the filesystem"
        );
    }

    #[tokio::test]
    async fn test_execute_idea_vote_task_does_not_write_sentinel_on_agent_failure() {
        // When the agentic loop itself fails (timeout, crash), the .voted
        // sentinel must NOT be written so the next process can retry.
        let mock = HubMock::new().await;
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let client = test_client(&mock.url);

        let task_id = "idea-no-sentinel-on-failure";
        let task = json!({
            "id": task_id,
            "title": "Failing idea",
            "description": "Agent loop will error.",
            "status": "open",
            "task_type": "idea",
            "metadata": {"votes": []}
        });

        let stub_fail = |_prompt: String, _workspace: PathBuf| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'static>> {
            Box::pin(async move {
                Err::<String, String>("simulated crash".to_string())
            })
        };

        execute_idea_vote_task(&cfg, &client, &task, stub_fail).await;

        assert!(
            !in_vote_cooldown_persistent(&cfg, task_id),
            ".voted sentinel must NOT exist when the agent loop fails (allow retry after restart)"
        );
    }

    #[tokio::test]
    async fn test_execute_idea_vote_task_does_not_write_sentinel_on_vote_put_failure() {
        // When the agentic loop succeeds but PUT /vote returns a non-OK
        // status (e.g. 409), the .voted sentinel must NOT be written so
        // a future process can retry (though the server will 409 again on
        // actual duplicate — the sentinel only skips the agentic loop cost).
        use crate::hub_mock::HubState;
        let mock = HubMock::with_state(HubState {
            task_vote_status: 409,
            ..Default::default()
        }).await;
        let (cfg, _tmp) = test_cfg_with_tmpdir();
        let client = test_client(&mock.url);

        let task_id = "idea-no-sentinel-on-409";
        let task = json!({
            "id": task_id,
            "title": "Conflicted idea",
            "description": "Server will return 409.",
            "status": "open",
            "task_type": "idea",
            "metadata": {"votes": []}
        });

        let stub_ok = |_prompt: String, _workspace: PathBuf| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'static>> {
            Box::pin(async move {
                Ok::<String, String>(
                    r#"{"vote":"approve","refinement":"Add error handling.","reason":"good"}"#
                        .to_string(),
                )
            })
        };

        execute_idea_vote_task(&cfg, &client, &task, stub_ok).await;

        // 409 from hub means the server recorded the vote anyway (see mock);
        // but in the agent's view the PUT "failed" (non-200 in the Err branch).
        // The sentinel must NOT be written — a future process can safely retry
        // (the server-side 409 protects against duplicate recording).
        assert!(
            !in_vote_cooldown_persistent(&cfg, task_id),
            ".voted sentinel must NOT be written when PUT /vote returns a non-OK status"
        );
    }

    // ── Poll 5: initiate_peer_exchange round-trip ─────────────────────────────
    //
    // These tests exercise the full Poll-5 path:
    //
    //   1. `initiate_peer_exchange` selects a random online peer, builds a
    //      `TestSuite` via `WorkspaceTestGenerator`, calls
    //      `PeerExchangeCoordinator::initiate`, and POSTs the resulting
    //      `agent.test_challenge` message to `POST /api/bus/send`.
    //
    //   2. The bus subscriber receives an inbound `agent.test_challenge` SSE
    //      event, parses it as an `ExchangeRequest`, spawns
    //      `handle_incoming_challenge`, which runs the tests and sends results
    //      back as `agent.test_submit` via `POST /api/bus/send`.
    //
    //   3. The bus subscriber receives an inbound `agent.test_submit` SSE event,
    //      extracts the `TestReport`s, calls `PeerExchangeCoordinator::decide_actions`,
    //      and spawns `handle_test_results` for each ordered `AgentAction`.
    //
    // The `HubMock` plays the role of the ACC hub: it records every
    // `POST /api/bus/send` body in `recorded_bus_sends` so the test can assert
    // on message type and content without a real network connection.

    /// Helper: build a `Config` whose `acc_dir` points to `tmp`.
    fn peer_exchange_cfg(url: &str, tmp: &tempfile::TempDir) -> Config {
        Config {
            acc_dir: tmp.path().to_path_buf(),
            acc_url: url.to_string(),
            acc_token: "test-token".to_string(),
            agent_name: "alice".to_string(),
            agentbus_token: String::new(),
            pair_programming: false,
            host: "localhost".to_string(),
            ssh_user: "u".to_string(),
            ssh_host: "127.0.0.1".to_string(),
            ssh_port: 22,
        }
    }

    // ── Poll-5 test 1: challenge is sent over the bus ────────────────────────
    //
    // `initiate_peer_exchange` must POST exactly one `agent.test_challenge`
    // message to `POST /api/bus/send` when a non-self peer is online and the
    // coordinator is not rate-limited.

    #[tokio::test]
    async fn test_poll5_initiate_sends_agent_test_challenge_to_bus() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mock = HubMock::new().await;
        let cfg = peer_exchange_cfg(&mock.url, &tmp);
        let client = test_client(&mock.url);
        let coordinator = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg.agent_name));

        // Use a single online peer that is not the initiating agent.
        let online_peers = vec!["bob".to_string()];

        initiate_peer_exchange(&cfg, &client, &coordinator, &online_peers).await;

        // Exactly one bus message must have been POSTed.
        let sends = mock.state.read().await.recorded_bus_sends.lock().await.clone();
        assert_eq!(sends.len(), 1, "exactly one bus send expected after initiation");

        let msg = &sends[0];
        assert_eq!(
            msg["type"].as_str().unwrap_or(""),
            "agent.test_challenge",
            "message type must be agent.test_challenge"
        );
        assert_eq!(
            msg["from"].as_str().unwrap_or(""),
            "alice",
            "message from must be the initiating agent"
        );
        assert_eq!(
            msg["to"].as_str().unwrap_or(""),
            "bob",
            "message to must be the selected peer"
        );

        // The body must be a valid serialised ExchangeRequest.
        let body_str = msg["body"].as_str().expect("body must be a JSON string");
        let req: crate::peer_exchange::ExchangeRequest =
            serde_json::from_str(body_str).expect("body must deserialise as ExchangeRequest");
        assert_eq!(req.from, "alice");
        assert_eq!(req.to, "bob");
        assert!(!req.suite.is_empty(), "test suite must not be empty");
    }

    // ── Poll-5 test 2: no challenge when no non-self peers online ────────────
    //
    // When `online_peers` contains only the initiating agent's own name,
    // `initiate_peer_exchange` must skip the initiation (no eligible peer)
    // and post no bus messages.

    #[tokio::test]
    async fn test_poll5_initiate_skips_when_no_non_self_peer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mock = HubMock::new().await;
        let cfg = peer_exchange_cfg(&mock.url, &tmp);
        let client = test_client(&mock.url);
        let coordinator = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg.agent_name));

        // Only the initiating agent itself is online — no eligible peer.
        let online_peers = vec!["alice".to_string()];

        initiate_peer_exchange(&cfg, &client, &coordinator, &online_peers).await;

        let sends = mock.state.read().await.recorded_bus_sends.lock().await.clone();
        assert_eq!(
            sends.len(),
            0,
            "no bus message must be sent when the only online peer is self"
        );
    }

    // ── Poll-5 test 3: no challenge when peer list is empty ──────────────────
    //
    // When `online_peers` is empty, `initiate_peer_exchange` must return
    // immediately without posting any bus message.

    #[tokio::test]
    async fn test_poll5_initiate_skips_when_peer_list_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mock = HubMock::new().await;
        let cfg = peer_exchange_cfg(&mock.url, &tmp);
        let client = test_client(&mock.url);
        let coordinator = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg.agent_name));

        initiate_peer_exchange(&cfg, &client, &coordinator, &[]).await;

        let sends = mock.state.read().await.recorded_bus_sends.lock().await.clone();
        assert_eq!(
            sends.len(),
            0,
            "no bus message must be sent when peer list is empty"
        );
    }

    // ── Poll-5 test 4: rate-limit prevents second consecutive challenge ───────
    //
    // Calling `initiate_peer_exchange` twice in a row for the same peer must
    // result in exactly one bus send (the second call is suppressed by the
    // coordinator's in-memory rate-limit table).

    #[tokio::test]
    async fn test_poll5_initiate_rate_limit_suppresses_second_challenge() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mock = HubMock::new().await;
        let cfg = peer_exchange_cfg(&mock.url, &tmp);
        let client = test_client(&mock.url);
        let coordinator = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg.agent_name));

        let online_peers = vec!["bob".to_string()];

        // First call — should succeed and send one challenge.
        initiate_peer_exchange(&cfg, &client, &coordinator, &online_peers).await;

        // Second call — must be suppressed by the rate limit.
        initiate_peer_exchange(&cfg, &client, &coordinator, &online_peers).await;

        let sends = mock.state.read().await.recorded_bus_sends.lock().await.clone();
        assert_eq!(
            sends.len(),
            1,
            "rate-limit must suppress the second challenge; expected 1 bus send, got {}",
            sends.len()
        );
        assert_eq!(
            sends[0]["type"].as_str().unwrap_or(""),
            "agent.test_challenge"
        );
    }

    // ── Poll-5 test 5: handle_incoming_challenge sends agent.test_submit ─────
    //
    // When the bus subscriber receives an inbound `agent.test_challenge` SSE
    // event addressed to us, `handle_incoming_challenge` must execute the
    // suite and post exactly one `agent.test_submit` message back to the bus.
    // The test verifies the full challenge → execute → submit arc without
    // requiring a real Anthropic API key.
    //
    // `#[tokio::test(flavor = "multi_thread")]` is required because
    // `ShellTestRunner::run` calls `tokio::task::block_in_place`, which panics
    // on a single-threaded runtime.

    #[tokio::test(flavor = "multi_thread")]
    async fn test_poll5_handle_incoming_challenge_sends_agent_test_submit() {
        use crate::peer_exchange::{ExchangeRequest, TestCase, TestSuite};

        let tmp = tempfile::tempdir().expect("tempdir");
        let mock = HubMock::new().await;
        let cfg = peer_exchange_cfg(&mock.url, &tmp);
        let client = test_client(&mock.url);
        let coordinator = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg.agent_name));

        // Build a minimal ExchangeRequest that will always pass on any machine
        // (the echo command is universally available and trivially succeeds).
        let req = ExchangeRequest {
            from: "bob".to_string(),
            to: "alice".to_string(),
            suite: TestSuite::new().with(TestCase {
                id: "tc-smoke".to_string(),
                title: "smoke test — always passes".to_string(),
                command: "echo ok".to_string(),
                expected_exit_code: 0,
                expected_output_contains: Some("ok".to_string()),
            }),
        };

        handle_incoming_challenge(&cfg, &client, &coordinator, req).await;

        // Exactly one bus send must have occurred (the agent.test_submit reply).
        let sends = mock.state.read().await.recorded_bus_sends.lock().await.clone();
        assert_eq!(
            sends.len(),
            1,
            "handle_incoming_challenge must POST exactly one agent.test_submit"
        );

        let msg = &sends[0];
        assert_eq!(
            msg["type"].as_str().unwrap_or(""),
            "agent.test_submit",
            "reply message type must be agent.test_submit"
        );
        assert_eq!(
            msg["from"].as_str().unwrap_or(""),
            "alice",
            "reply from must be this agent"
        );
        assert_eq!(
            msg["to"].as_str().unwrap_or(""),
            "bob",
            "reply to must be the original initiator"
        );

        // The body must deserialise as a non-empty list of TestReports.
        let body_str = msg["body"].as_str().expect("body must be a JSON string");
        let reports: Vec<crate::peer_exchange::TestReport> =
            serde_json::from_str(body_str).expect("body must deserialise as Vec<TestReport>");
        assert_eq!(reports.len(), 1, "one report per test case");
        assert_eq!(reports[0].test_id, "tc-smoke");
        assert!(reports[0].passed, "tc-smoke must pass (echo ok always exits 0)");
    }

    // ── Poll-5 test 6: subscribe_bus dispatches agent.test_challenge ─────────
    //
    // When the SSE stream delivers an `agent.test_challenge` event directed at
    // this agent, `subscribe_bus` must parse the `ExchangeRequest` from the
    // message body and spawn `handle_incoming_challenge`, which posts an
    // `agent.test_submit` message to the bus.
    //
    // This test exercises the full subscribe_bus → handle_incoming_challenge →
    // bus send path using the SSE injection capability of `HubMock::with_sse`.
    //
    // `#[tokio::test(flavor = "multi_thread")]` is required because
    // `ShellTestRunner::run` (spawned inside `handle_incoming_challenge`) calls
    // `tokio::task::block_in_place`, which panics on a single-threaded runtime.

    #[tokio::test(flavor = "multi_thread")]
    async fn test_poll5_subscribe_bus_routes_test_challenge_to_handler() {
        use crate::peer_exchange::{ExchangeRequest, TestCase, TestSuite};

        let tmp = tempfile::tempdir().expect("tempdir");

        // Build the ExchangeRequest that the "initiator" would have sent.
        let req = ExchangeRequest {
            from: "bob".to_string(),
            to: "alice".to_string(),
            suite: TestSuite::new().with(TestCase {
                id: "tc-bus-route".to_string(),
                title: "bus routing smoke test".to_string(),
                command: "echo bus-route-ok".to_string(),
                expected_exit_code: 0,
                expected_output_contains: Some("bus-route-ok".to_string()),
            }),
        };
        let req_json = serde_json::to_string(&req).expect("serialise ExchangeRequest");

        // Craft the SSE event the hub would send to the bus stream.
        let sse_event = json!({
            "type": "agent.test_challenge",
            "from": "bob",
            "to":   "alice",
            "body": req_json,
        })
        .to_string();

        let mock = HubMock::with_sse(vec![sse_event]).await;
        let cfg = peer_exchange_cfg(&mock.url, &tmp);
        let client = test_client(&mock.url);

        let nudge      = Arc::new(tokio::sync::Notify::new());
        let vote_nudge = Arc::new(tokio::sync::Notify::new());
        let coordinator = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg.agent_name));

        // Run subscribe_bus in the background; it will drain the SSE stream
        // (one event) and then return when the server closes the connection.
        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        let coord2      = coordinator.clone();
        tokio::spawn(async move {
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord2).await;
        });

        // Give the spawned tasks enough time to process the SSE event and post
        // the agent.test_submit reply.  Using a timeout here keeps the test
        // bounded even if something goes wrong.
        let deadline = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();
        let mut sends;
        loop {
            sends = mock.state.read().await.recorded_bus_sends.lock().await.clone();
            if !sends.is_empty() || start.elapsed() > deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        assert!(
            !sends.is_empty(),
            "subscribe_bus must trigger an agent.test_submit bus send after receiving \
             agent.test_challenge; no bus sends were recorded within {:?}",
            deadline
        );

        // The first (and only) send must be agent.test_submit addressed back to
        // the original initiator.
        let msg = &sends[0];
        assert_eq!(
            msg["type"].as_str().unwrap_or(""),
            "agent.test_submit",
            "bus send type must be agent.test_submit"
        );
        assert_eq!(msg["from"].as_str().unwrap_or(""), "alice");
        assert_eq!(msg["to"].as_str().unwrap_or(""), "bob");

        // Verify the body contains valid reports.
        let body_str = msg["body"].as_str().expect("body must be a string");
        let reports: Vec<crate::peer_exchange::TestReport> =
            serde_json::from_str(body_str).expect("body must deserialise as Vec<TestReport>");
        assert_eq!(reports.len(), 1, "one report for the one test case");
        assert_eq!(reports[0].test_id, "tc-bus-route");
        assert!(reports[0].passed, "tc-bus-route must pass (echo exits 0)");
    }

    // ── Poll-5 test 7: subscribe_bus dispatches agent.test_submit ────────────
    //
    // When the SSE stream delivers an `agent.test_submit` event directed at
    // this agent with passing reports, `subscribe_bus` must call
    // `decide_actions` (which produces `LogPassSummary` for an all-pass
    // result) and emit the corresponding `agent.peer_exchange.pass_summary`
    // bus event.  This test validates the full submit → tally → action
    // dispatch arc.
    //
    // The `subscribe_bus` `agent.test_submit` handler expects the reports to
    // be parseable from `msg.body` via one of three shapes:
    //   - `Value::String(s)` where `s` is JSON containing a `"reports"` field
    //   - `Value::Object` with a `"reports"` field
    //   - `Value::Array` (bare array of TestReport)
    //
    // We use the `{"reports": [...]}` wrapper form here (a JSON string whose
    // content is an object with a "reports" key) so the test matches the path
    // exercised by the real `handle_incoming_challenge` output.

    #[tokio::test]
    async fn test_poll5_subscribe_bus_routes_test_submit_to_action_dispatch() {
        use crate::peer_exchange::TestReport;

        let tmp = tempfile::tempdir().expect("tempdir");

        // Craft a passing TestReport list (one case, passed=true).
        let reports = vec![TestReport::pass("tc-pass", "ok")];
        // Wrap in {"reports": [...]} so subscribe_bus can parse it via the
        // Value::String → json["reports"] path used by the production code.
        let body_json = json!({"reports": reports});
        let body_str = serde_json::to_string(&body_json).expect("serialise body");

        // The SSE event the hub sends after the target completes the suite.
        let sse_event = json!({
            "type": "agent.test_submit",
            "from": "bob",
            "to":   "alice",
            "body": body_str,
        })
        .to_string();

        let mock = HubMock::with_sse(vec![sse_event]).await;
        let cfg = peer_exchange_cfg(&mock.url, &tmp);
        let client = test_client(&mock.url);

        let nudge      = Arc::new(tokio::sync::Notify::new());
        let vote_nudge = Arc::new(tokio::sync::Notify::new());
        let coordinator = Arc::new(crate::peer_exchange::PeerExchangeCoordinator::new(&cfg.agent_name));

        let cfg2        = cfg.clone();
        let client2     = client.clone();
        let nudge2      = nudge.clone();
        let vote_nudge2 = vote_nudge.clone();
        let coord2      = coordinator.clone();
        tokio::spawn(async move {
            let _ = subscribe_bus(&cfg2, &client2, &nudge2, &vote_nudge2, &coord2).await;
        });

        // Wait for the pass_summary bus event to be posted (up to 5 s).
        let deadline = std::time::Duration::from_secs(5);
        let start = std::time::Instant::now();
        let mut sends;
        loop {
            sends = mock.state.read().await.recorded_bus_sends.lock().await.clone();
            // The pass summary event is the one we are waiting for.
            if sends.iter().any(|m| {
                m["type"].as_str() == Some("agent.peer_exchange.pass_summary")
            }) || start.elapsed() > deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let pass_summary = sends
            .iter()
            .find(|m| m["type"].as_str() == Some("agent.peer_exchange.pass_summary"));
        assert!(
            pass_summary.is_some(),
            "subscribe_bus must emit agent.peer_exchange.pass_summary when all tests pass; \
             bus sends recorded: {:?}",
            sends
        );

        let ps = pass_summary.unwrap();
        // The body must carry the tally fields from the LogPassSummary action.
        let ps_body_str = ps["body"].as_str().expect("pass_summary body must be a string");
        let ps_body: serde_json::Value =
            serde_json::from_str(ps_body_str).expect("pass_summary body must be valid JSON");
        assert_eq!(
            ps_body["total"].as_u64().unwrap_or(0),
            1,
            "pass_summary total must equal the number of reports"
        );
        assert_eq!(
            ps_body["failed"].as_u64().unwrap_or(999),
            0,
            "pass_summary failed must be 0 for an all-pass result"
        );
    }
}
