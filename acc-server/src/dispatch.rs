//! Auto-dispatch loop — routes unclaimed fleet tasks to available agents.
//!
//! Runs as a background Tokio task. Three escalating phases per unclaimed task:
//!   1. Directed nudge → best-matched online agent (immediate on create, repeated each tick)
//!   2. Broadcast nudge → all capable agents (after NUDGE_AFTER seconds)
//!   3. Explicit server-side claim → picks agent, claims atomically (after ASSIGN_AFTER seconds)
//!
//! Backfill: tasks older than BACKFILL_THRESHOLD skip straight to phase 3 on first tick.
//! Idle discovery: agents with no work get discovery tasks auto-assigned.
//! Idea voting: open idea tasks nudge eligible voters; tallied for promotion/rejection.
//! Rocky escalation: ideas near expiry ask the user before archiving.

use crate::AppState;
use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

// ── Config ────────────────────────────────────────────────────────────────────

pub struct DispatchConfig {
    pub enabled: bool,
    pub tick_secs: u64,
    pub nudge_after_secs: i64,
    pub assign_after_secs: i64,
    pub max_assign_attempts: i64,
    pub backfill_threshold_secs: i64,
    pub idle_grace_period_secs: i64,
    pub idea_approve_threshold: usize,
    pub idea_reject_threshold: usize,
    pub idea_vote_expiry_secs: i64,
    pub idea_expiry_warn_before_secs: i64,
    pub rocky_response_timeout_secs: i64,
    /// Max tasks to explicitly assign to any single agent per tick (prevents bulk pile-on)
    pub max_tasks_per_agent: usize,
}

impl DispatchConfig {
    pub fn from_env() -> Self {
        Self {
            enabled:                   env_bool("ACC_DISPATCH_ENABLED", true),
            tick_secs:                 env_u64("ACC_DISPATCH_TICK", 15),
            nudge_after_secs:          env_i64("ACC_DISPATCH_NUDGE_AFTER", 30),
            assign_after_secs:         env_i64("ACC_DISPATCH_ASSIGN_AFTER", 90),
            max_assign_attempts:       env_i64("ACC_DISPATCH_MAX_ASSIGN_ATTEMPTS", 3),
            backfill_threshold_secs:   env_i64("ACC_DISPATCH_BACKFILL_THRESHOLD", 3600),
            idle_grace_period_secs:    env_i64("ACC_IDLE_GRACE_PERIOD", 120),
            idea_approve_threshold:    env_usize("ACC_IDEA_APPROVE_THRESHOLD", 3),
            idea_reject_threshold:     env_usize("ACC_IDEA_REJECT_THRESHOLD", 2),
            idea_vote_expiry_secs:     env_i64("ACC_IDEA_VOTE_EXPIRY", 604_800),
            idea_expiry_warn_before_secs: env_i64("ACC_IDEA_EXPIRY_WARN_BEFORE", 86_400),
            rocky_response_timeout_secs:  env_i64("ACC_ROCKY_RESPONSE_TIMEOUT", 14_400),
            max_tasks_per_agent:          env_usize("ACC_MAX_TASKS_PER_AGENT", 2),
        }
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| v != "false" && v != "0")
        .unwrap_or(default)
}
fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run(state: Arc<AppState>) {
    let cfg = DispatchConfig::from_env();
    if !cfg.enabled {
        info!("[dispatch] disabled (ACC_DISPATCH_ENABLED=false)");
        return;
    }
    info!("[dispatch] tick loop started (tick={}s nudge={}s assign={}s backfill={}s)",
        cfg.tick_secs, cfg.nudge_after_secs, cfg.assign_after_secs, cfg.backfill_threshold_secs);

    let mut bus_rx = state.bus_tx.subscribe();
    let mut interval = tokio::time::interval(
        std::time::Duration::from_secs(cfg.tick_secs)
    );
    loop {
        tokio::select! {
            _ = interval.tick() => {
                tick(&state, &cfg).await;
            }
            msg = bus_rx.recv() => {
                match msg {
                    Ok(s) => handle_bus_message(&state, &cfg, &s).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        info!("[dispatch] bus lagged, dropped {} messages", n);
                    }
                    Err(_) => {}
                }
            }
        }
    }
}

async fn handle_bus_message(state: &Arc<AppState>, _cfg: &DispatchConfig, msg: &str) {
    let v: Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(_) => return,
    };
    if v["type"].as_str() != Some("rocky:human_response") { return; }
    let task_id = match v["idea_task_id"].as_str() { Some(id) => id.to_string(), None => return };
    let action = match v["action"].as_str() { Some(a) => a.to_string(), None => return };
    let now = Utc::now();

    match action.as_str() {
        "extend_7d" => {
            update_task_meta_field(state, &task_id, "expiry_extended_at", json!(now.to_rfc3339())).await;
            info!("[dispatch] rocky: extended 7d idea={}", task_id);
        }
        "promote_anyway" => {
            let ideas = fetch_open_ideas(state).await;
            if let Some(idea) = ideas.iter().find(|i| i["id"].as_str() == Some(task_id.as_str())) {
                let votes = idea["metadata"]["votes"].as_array().cloned().unwrap_or_default();
                let approvals: Vec<&Value> = votes.iter()
                    .filter(|v| v["vote"].as_str() == Some("approve")
                        && !v["refinement"].as_str().unwrap_or("").is_empty())
                    .collect();
                promote_idea(state, idea, &approvals, now).await;
            }
            info!("[dispatch] rocky: promoted anyway idea={}", task_id);
        }
        "let_expire" => {
            reject_idea(state, &task_id, now).await;
            info!("[dispatch] rocky: let expire idea={}", task_id);
        }
        _ => {}
    }
}

async fn tick(state: &Arc<AppState>, cfg: &DispatchConfig) {
    let agents_snapshot = state.agents.read().await.clone();
    let now = Utc::now();

    // Mutable so we can track in-tick assignments and avoid pile-on
    let mut claimed_counts = get_claimed_counts(state).await;

    // Fetch all open, non-discovery, non-idea tasks (for dispatch phases 1-3)
    let open_tasks = fetch_open_dispatchable_tasks(state).await;

    for task in &open_tasks {
        if let Some(assigned) = dispatch_task(state, cfg, task, &agents_snapshot, &claimed_counts, now).await {
            *claimed_counts.entry(assigned).or_insert(0) += 1;
        }
    }

    // Idea voting nudges and tally
    tally_idea_votes(state, cfg, &agents_snapshot, now).await;

    // Rocky pre-expiry warnings
    check_idea_expiry(state, cfg, now).await;

    // Idle agent discovery
    detect_and_assign_discovery(state, cfg, &agents_snapshot, &claimed_counts, now).await;

    // Auto-file phase_commit tasks for projects whose AgentFS is dirty
    // and don't already have a milestone-commit task in flight.
    auto_file_phase_commits(state).await;

    // Auto-unclaim tasks whose claim_expires_at is in the past — the
    // agent that claimed them either died, lost network, or never
    // posted progress. Defense in depth alongside agent-side
    // RECLAIM_COOLDOWN + heartbeat (CCC-t9b).
    sweep_expired_claims(state).await;

    // Drift-fix #1: pull origin/main into clean AgentFS workspaces so
    // they don't lag behind the actual git state. Skip dirty projects
    // and projects with in-flight tasks (don't yank the rug).
    auto_refresh_workspaces(state).await;
}

// ── Workspace refresh (drift-fix #1) ─────────────────────────────────────
//
// Once a project is git-cloned into AgentFS at create time, nothing
// pulls origin/main again. If a human or CI pushes to main via a PR,
// AgentFS stays frozen on the old clone state and agents work against
// stale source. This periodic refresh closes that drift mode.
//
// Interval is intentionally coarse (REFRESH_TICK_INTERVAL_SECS) so we
// don't fight phase_commit pushes or hammer remote git.

const REFRESH_TICK_INTERVAL_SECS: i64 = 600; // every 10 minutes

async fn auto_refresh_workspaces(state: &Arc<AppState>) {
    // Throttle: only run on ticks that align with the refresh interval.
    // tick() runs every cfg.tick_secs (default 15s); we run roughly
    // once per REFRESH_TICK_INTERVAL_SECS.
    let now_unix = Utc::now().timestamp();
    if now_unix % REFRESH_TICK_INTERVAL_SECS >= 15 {
        return;
    }

    let projects = state.projects.read().await.clone();
    for project in projects.iter() {
        let project_id = match project.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        let path = match project.get("agentfs_path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => continue,
        };
        let status = project.get("status").and_then(|v| v.as_str()).unwrap_or("active");
        if status != "active" { continue; }
        let dirty = project.get("agentfs_dirty").and_then(|v| v.as_bool()).unwrap_or(false);
        if dirty { continue; } // never overwrite uncommitted work
        let clone_status = project.get("clone_status").and_then(|v| v.as_str()).unwrap_or("");
        if clone_status != "ready" { continue; } // not yet cloned, or failed

        // Skip if any task for this project is currently in flight —
        // an agent is actively working in this directory and a pull
        // mid-task could surprise its bash/editor tool calls.
        let in_flight: i64 = {
            let db = state.fleet_db.lock().await;
            db.query_row(
                "SELECT COUNT(*) FROM fleet_tasks
                 WHERE project_id=?1 AND status IN ('claimed','in_progress')",
                params![project_id],
                |row| row.get(0),
            ).unwrap_or(0)
        };
        if in_flight > 0 { continue; }

        match crate::routes::projects::git_pull_workspace(&path).await {
            Ok(s) if s == "already up to date" => {
                // Quiet success — don't spam the log every 10 min for clean projects
            }
            Ok(s) => {
                info!("[dispatch] refreshed AgentFS for project {project_id}: {s}");
                let _ = state.bus_tx.send(json!({
                    "type":"projects:refreshed","project_id":project_id,"summary":s,"source":"auto"
                }).to_string());
            }
            Err(e) => {
                // Diverged or fetch failure — log but don't escalate; the
                // operator can investigate. We never do non-FF merges
                // automatically.
                info!("[dispatch] refresh failed for project {project_id}: {e}");
            }
        }
    }
}

// ── Stale-claim sweeper (CCC-t9b) ─────────────────────────────────────────
//
// claim_task sets claim_expires_at = now + 4h. Without enforcement, dead
// agents hold tasks forever and capacity never frees up. Unclaim any task
// whose expiry has passed; the dispatch loop will re-route it on the
// next tick.
async fn sweep_expired_claims(state: &Arc<AppState>) {
    let now_str = Utc::now().to_rfc3339();
    let unclaimed: Vec<(String, String)> = {
        let db = state.fleet_db.lock().await;
        let mut stmt = match db.prepare(
            "SELECT id, COALESCE(claimed_by,'') FROM fleet_tasks
             WHERE status IN ('claimed','in_progress')
               AND claim_expires_at IS NOT NULL
               AND claim_expires_at < ?1"
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        let rows = stmt.query_map(params![now_str], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        });
        let rows = match rows { Ok(r) => r, Err(_) => return };
        rows.filter_map(|r| r.ok()).collect()
    };

    if unclaimed.is_empty() { return; }

    let db = state.fleet_db.lock().await;
    for (id, prev_agent) in &unclaimed {
        let _ = db.execute(
            "UPDATE fleet_tasks SET status='open', claimed_by=NULL, claimed_at=NULL,
                claim_expires_at=NULL, updated_at=?1
             WHERE id=?2 AND status IN ('claimed','in_progress')
               AND claim_expires_at IS NOT NULL AND claim_expires_at < ?1",
            params![now_str, id],
        );
        info!("[dispatch] swept expired claim task={} prev_agent={}", id, prev_agent);
        let _ = state.bus_tx.send(json!({
            "type": "tasks:unclaimed",
            "task_id": id,
            "agent": prev_agent,
            "reason": "claim_expired",
        }).to_string());
    }
}

// ── Auto-phase-commit (CCC-amn) ───────────────────────────────────────────
//
// Per the CCC-tk0 lifecycle, AgentFS state is committed and pushed back to
// git only by a phase_commit task. Without something *filing* those tasks,
// dirty bits accumulate forever (observed: natasha alone sitting on 532
// modified lines across 6 projects, never pushed). This phase scans
// projects each tick and queues a phase_commit task for any active project
// that's dirty AND doesn't already have one in flight.

async fn auto_file_phase_commits(state: &Arc<AppState>) {
    let projects = state.projects.read().await.clone();

    for project in projects.iter() {
        let dirty = project.get("agentfs_dirty").and_then(|v| v.as_bool()).unwrap_or(false);
        if !dirty {
            continue;
        }
        let project_id = match project.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => continue,
        };
        let project_status = project.get("status").and_then(|v| v.as_str()).unwrap_or("active");
        if project_status != "active" {
            continue; // skip archived projects
        }

        // Drift-fix #4: stop auto-filing if the last 3 phase_commits for
        // this project failed in a row. Operator must POST /clean
        // manually (or fix the underlying problem and reset) before we
        // resume. Prevents queueing dozens of doomed tasks.
        let consecutive_failures = project.get("phase_commit_consecutive_failures")
            .and_then(|v| v.as_i64()).unwrap_or(0);
        if consecutive_failures >= 3 {
            // Log once per project per 10 ticks (~150s) so the alert is
            // visible without spamming.
            if Utc::now().timestamp() % 150 < 15 {
                info!(
                    "[dispatch] phase_commit auto-fill paused for project {project_id} \
                     — {consecutive_failures} consecutive failures; reset via /clean or manual fix"
                );
            }
            continue;
        }

        // Skip if a phase_commit task is already pending or in flight for
        // this project. Prevents pile-on across ticks.
        let already_in_flight: i64 = {
            let db = state.fleet_db.lock().await;
            db.query_row(
                "SELECT COUNT(*) FROM fleet_tasks
                 WHERE project_id=?1
                   AND task_type='phase_commit'
                   AND status IN ('open','claimed','in_progress')",
                params![project_id],
                |row| row.get(0),
            ).unwrap_or(0)
        };
        if already_in_flight > 0 {
            continue;
        }

        // File a new phase_commit task
        let task_id = format!("task-{}", uuid::Uuid::new_v4().simple());
        let project_name = project.get("name").and_then(|v| v.as_str()).unwrap_or("project").to_string();
        let title = format!("phase_commit: {project_name}");
        let description = format!(
            "Auto-filed milestone-commit task. The project's AgentFS workspace \
             has accumulated uncommitted edits. Commit them (any branch is \
             fine — phase_commit handler creates phase/{{phase}} and pushes), \
             then call POST /api/projects/{project_id}/clean to clear the \
             dirty bit.\n\n\
             Source: dispatch.rs auto_file_phase_commits"
        );
        let metadata = json!({
            "source": "auto-phase-commit",
            "auto_filed_at": Utc::now().to_rfc3339(),
        }).to_string();
        let phase = "milestone";

        let inserted = {
            let db = state.fleet_db.lock().await;
            db.execute(
                "INSERT INTO fleet_tasks
                  (id, project_id, title, description, priority, metadata, task_type, phase, blocked_by)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, 'phase_commit', ?6, '[]')",
                params![task_id, project_id, title, description, metadata, phase],
            )
        };
        match inserted {
            Ok(_) => {
                info!("[dispatch] auto-filed phase_commit task {} for project {} ({})",
                    task_id, project_id, project_name);
                let _ = state.bus_tx.send(
                    json!({
                        "type": "tasks:added",
                        "task_id": task_id,
                        "task_type": "phase_commit",
                        "project_id": project_id,
                        "auto_filed": true,
                    }).to_string()
                );
            }
            Err(e) => {
                info!("[dispatch] auto-file phase_commit failed for project {}: {e}", project_id);
            }
        }
    }
}

// ── Phase dispatcher ──────────────────────────────────────────────────────────

/// Returns the agent name if a phase-3 explicit assignment was made, else None.
async fn dispatch_task(
    state: &Arc<AppState>,
    cfg: &DispatchConfig,
    task: &Value,
    agents: &Value,
    claimed_counts: &HashMap<String, usize>,
    now: DateTime<Utc>,
) -> Option<String> {
    let task_id = match task["id"].as_str() { Some(id) => id, None => return None };
    let created_at = match task["created_at"].as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
    {
        Some(dt) => dt,
        None => return None,
    };

    let dispatch_meta = task["metadata"]["dispatch"].as_object();
    let assign_attempts: i64 = dispatch_meta
        .and_then(|m| m.get("assign_attempts")).and_then(|v| v.as_i64()).unwrap_or(0);
    let last_nudge_at: Option<DateTime<Utc>> = dispatch_meta
        .and_then(|m| m.get("last_nudge_at")).and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let blacklist: Vec<String> = dispatch_meta
        .and_then(|m| m.get("blacklist"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if assign_attempts >= cfg.max_assign_attempts {
        return None; // gave up on this task
    }

    let age_secs = (now - created_at).num_seconds();
    let is_backfill = age_secs >= cfg.backfill_threshold_secs;

    // Phase 3: explicit assign (backfill or aged past assign threshold)
    let nudge_age = last_nudge_at.map(|t| (now - t).num_seconds()).unwrap_or(i64::MAX);
    let past_assign = nudge_age >= cfg.assign_after_secs || is_backfill;

    if past_assign {
        if let Some(agent) = select_best_agent(task, agents, claimed_counts, &blacklist, cfg.max_tasks_per_agent) {
            explicit_assign(state, task_id, &agent, now).await;
            info!("[dispatch] phase3 assign task={} agent={}", task_id, agent);
            return Some(agent);
        } else {
            // no capable agent with capacity — fall back to broadcast nudge
            publish_nudge(state, task_id, task, None);
            update_nudge_meta(state, task_id, task, now).await;
            info!("[dispatch] phase2 broadcast (no capable agent with capacity) task={}", task_id);
        }
        return None;
    }

    // Phase 2: broadcast nudge (past nudge threshold, not yet assign threshold)
    let past_nudge = last_nudge_at.map(|t| (now - t).num_seconds() >= cfg.nudge_after_secs)
        .unwrap_or(true);

    if past_nudge {
        // Try directed first; fall back to broadcast if no match
        let target = select_best_agent(task, agents, claimed_counts, &blacklist, cfg.max_tasks_per_agent);
        publish_nudge(state, task_id, task, target.as_deref());
        update_nudge_meta(state, task_id, task, now).await;
        if let Some(ref a) = target {
            info!("[dispatch] phase2 directed-nudge task={} agent={}", task_id, a);
        } else {
            info!("[dispatch] phase2 broadcast task={}", task_id);
        }
    }
    None
}

// ── Capability matching (pure — no I/O) ──────────────────────────────────────

/// Select the best online agent for a task.
/// Pure function: takes snapshots, returns agent name or None.
/// Rejects agents whose claimed count is already at or above `max_per_agent`.
pub fn select_best_agent(
    task: &Value,
    agents: &Value,
    claimed_counts: &HashMap<String, usize>,
    blacklist: &[String],
    max_per_agent: usize,
) -> Option<String> {
    let required_executor = task["metadata"]["preferred_executor"].as_str()
        .filter(|s| !s.is_empty());

    let mut candidates: Vec<(String, usize)> = agents.as_object()?
        .iter()
        .filter_map(|(name, agent)| {
            if !is_agent_online(agent) { return None; }
            if blacklist.contains(name) { return None; }
            let load = *claimed_counts.get(name).unwrap_or(&0);
            if load >= max_per_agent { return None; }
            if let Some(req) = required_executor {
                let caps = &agent["capabilities"];
                let has_cap = caps.get(req)
                    .map(|v| v.as_bool().unwrap_or(false) || v.as_str().map(|s| !s.is_empty()).unwrap_or(false))
                    .unwrap_or(false);
                if !has_cap { return None; }
            }
            Some((name.clone(), load))
        })
        .collect();

    if candidates.is_empty() { return None; }

    // Sort: least loaded first, alphabetical tiebreak
    candidates.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    Some(candidates.into_iter().next()?.0)
}

/// Check if an agent is online (lastSeen within 300s).
pub fn is_agent_online(agent: &Value) -> bool {
    agent.get("lastSeen")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| (Utc::now() - dt.with_timezone(&Utc)).num_seconds() < 300)
        .unwrap_or(false)
}

// ── Directed/broadcast nudge on task create ───────────────────────────────────

/// Called from routes/tasks.rs immediately after a task is inserted.
pub async fn nudge_new_task(state: &Arc<AppState>, task: &Value) {
    let task_id = match task["id"].as_str() { Some(id) => id, None => return };
    let agents = state.agents.read().await;
    let claimed_counts = get_claimed_counts(state).await;
    let max_per_agent = std::env::var("ACC_MAX_TASKS_PER_AGENT")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(2usize);
    let target = select_best_agent(task, &agents, &claimed_counts, &[], max_per_agent);
    publish_nudge(state, task_id, task, target.as_deref());
}

fn publish_nudge(state: &Arc<AppState>, task_id: &str, task: &Value, to: Option<&str>) {
    let mut msg = json!({
        "type": "tasks:dispatch_nudge",
        "task_id": task_id,
        "project_id": task["project_id"],
        "task_type": task["task_type"],
        "priority": task["priority"],
    });
    if let Some(agent) = to {
        msg["to"] = json!(agent);
    }
    let _ = state.bus_tx.send(msg.to_string());
}

// ── Explicit server-side claim (phase 3) ──────────────────────────────────────

async fn explicit_assign(state: &Arc<AppState>, task_id: &str, agent: &str, now: DateTime<Utc>) {
    let now_str = now.to_rfc3339();
    let expires_str = (now + Duration::hours(4)).to_rfc3339();

    let rows = {
        let db = state.fleet_db.lock().await;
        db.execute(
            "UPDATE fleet_tasks SET status='claimed', claimed_by=?1, claimed_at=?2, claim_expires_at=?3, updated_at=?2 \
             WHERE id=?4 AND status='open'",
            params![agent, now_str, expires_str, task_id],
        ).unwrap_or(0)
    };

    if rows > 0 {
        // Increment assign_attempts in metadata
        update_assign_meta(state, task_id).await;
        let _ = state.bus_tx.send(json!({
            "type": "tasks:dispatch_assigned",
            "to": agent,
            "task_id": task_id,
        }).to_string());
        let _ = state.bus_tx.send(json!({
            "type": "tasks:claimed",
            "task_id": task_id,
            "agent": agent,
        }).to_string());
    }
}

// ── Idea voting nudges ────────────────────────────────────────────────────────

async fn tally_idea_votes(
    state: &Arc<AppState>,
    cfg: &DispatchConfig,
    agents: &Value,
    now: DateTime<Utc>,
) {
    let ideas = fetch_open_ideas(state).await;

    for idea in &ideas {
        let task_id = match idea["id"].as_str() { Some(id) => id, None => continue };

        let votes = idea["metadata"]["votes"].as_array().cloned().unwrap_or_default();
        let creator = idea["metadata"]["created_by"].as_str().unwrap_or("");

        let approvals: Vec<&Value> = votes.iter()
            .filter(|v| v["vote"].as_str() == Some("approve")
                && !v["refinement"].as_str().unwrap_or("").is_empty())
            .collect();
        let rejections: usize = votes.iter()
            .filter(|v| v["vote"].as_str() == Some("reject"))
            .count();

        if approvals.len() >= cfg.idea_approve_threshold {
            promote_idea(state, idea, &approvals, now).await;
            continue;
        }
        if rejections >= cfg.idea_reject_threshold {
            reject_idea(state, task_id, now).await;
            continue;
        }

        // Nudge eligible voters
        let voted_agents: Vec<&str> = votes.iter()
            .filter_map(|v| v["agent"].as_str())
            .collect();

        let eligible: Vec<String> = agents.as_object()
            .map(|obj| obj.iter()
                .filter_map(|(name, agent)| {
                    if !is_agent_online(agent) { return None; }
                    if name == creator { return None; }
                    if voted_agents.contains(&name.as_str()) { return None; }
                    Some(name.clone())
                })
                .collect())
            .unwrap_or_default();

        for agent_name in &eligible {
            let _ = state.bus_tx.send(json!({
                "type": "tasks:dispatch_nudge",
                "to": agent_name,
                "task_id": task_id,
                "task_type": "vote",
                "project_id": idea["project_id"],
                "priority": idea["priority"],
            }).to_string());
        }
    }
}

async fn promote_idea(
    state: &Arc<AppState>,
    idea: &Value,
    approvals: &[&Value],
    now: DateTime<Utc>,
) {
    let idea_id = match idea["id"].as_str() { Some(id) => id, None => return };
    let now_str = now.to_rfc3339();

    // Build merged description
    let base_desc = idea["description"].as_str().unwrap_or("");
    let refinements: String = approvals.iter()
        .filter_map(|v| {
            let agent = v["agent"].as_str().unwrap_or("?");
            let r = v["refinement"].as_str().unwrap_or("").trim();
            if r.is_empty() { None } else { Some(format!("- **{}**: {}", agent, r)) }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let full_desc = if refinements.is_empty() {
        base_desc.to_string()
    } else {
        format!("{}\n\n---\n**Agent refinements:**\n{}", base_desc, refinements)
    };

    let new_id = format!("task-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let project_id = idea["project_id"].as_str().unwrap_or("");
    let title = idea["title"].as_str().unwrap_or("Promoted idea");
    let priority = idea["priority"].as_i64().unwrap_or(2);
    let meta = json!({ "promoted_from": idea_id }).to_string();

    {
        let db = state.fleet_db.lock().await;
        let _ = db.execute(
            "INSERT INTO fleet_tasks (id,project_id,title,description,priority,metadata,task_type,phase,blocked_by) \
             VALUES (?1,?2,?3,?4,?5,?6,'work','build','[]')",
            params![new_id, project_id, title, full_desc, priority, meta],
        );
        let completed_meta = {
            let raw: String = db.query_row(
                "SELECT metadata FROM fleet_tasks WHERE id=?1", params![idea_id], |r| r.get(0)
            ).unwrap_or_else(|_| "{}".to_string());
            let mut m: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            m["promoted"] = json!(true);
            m.to_string()
        };
        let _ = db.execute(
            "UPDATE fleet_tasks SET status='completed', completed_at=?1, metadata=?2, updated_at=?1 WHERE id=?3",
            params![now_str, completed_meta, idea_id],
        );
    }

    let _ = state.bus_tx.send(json!({
        "type": "tasks:added",
        "task_id": new_id,
        "project_id": project_id,
        "promoted_from": idea_id,
    }).to_string());
    info!("[dispatch] idea promoted idea={} → work={}", idea_id, new_id);
}

async fn reject_idea(state: &Arc<AppState>, task_id: &str, now: DateTime<Utc>) {
    let db = state.fleet_db.lock().await;
    let _ = db.execute(
        "UPDATE fleet_tasks SET status='rejected', updated_at=?1 WHERE id=?2",
        params![now.to_rfc3339(), task_id],
    );
    info!("[dispatch] idea rejected task={}", task_id);
}

// ── Rocky pre-expiry escalation ───────────────────────────────────────────────

async fn check_idea_expiry(state: &Arc<AppState>, cfg: &DispatchConfig, now: DateTime<Utc>) {
    let warn_threshold = Duration::seconds(cfg.idea_vote_expiry_secs - cfg.idea_expiry_warn_before_secs);
    let expire_threshold = Duration::seconds(cfg.idea_vote_expiry_secs);
    let rocky_timeout = Duration::seconds(cfg.rocky_response_timeout_secs);

    let ideas = fetch_open_ideas(state).await;

    for idea in &ideas {
        let task_id = match idea["id"].as_str() { Some(id) => id, None => continue };
        let created_at = match idea["created_at"].as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        {
            Some(dt) => dt.with_timezone(&Utc),
            None => continue,
        };

        let age = now - created_at;
        let meta = &idea["metadata"];
        let expiry_warned = meta["expiry_warned"].as_bool().unwrap_or(false);
        let extended_at = meta["expiry_extended_at"].as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        // Effective expiry: base + optional 7-day extension
        let effective_expiry = expire_threshold
            + extended_at.map(|_| Duration::days(7)).unwrap_or(Duration::zero());

        // Hard expire after timeout waiting for Rocky
        if expiry_warned {
            let warn_sent_at = meta["rocky_warn_sent_at"].as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            if let Some(sent) = warn_sent_at {
                if age > effective_expiry || (now - sent) > rocky_timeout {
                    reject_idea(state, task_id, now).await;
                    info!("[dispatch] idea expired (Rocky timeout/expiry) task={}", task_id);
                }
            }
            continue;
        }

        // Warn Rocky when approaching expiry
        if age > warn_threshold && age < effective_expiry {
            let votes = idea["metadata"]["votes"].as_array().cloned().unwrap_or_default();
            let vote_summary = votes.iter()
                .map(|v| format!("- {} ({}): {}",
                    v["agent"].as_str().unwrap_or("?"),
                    v["vote"].as_str().unwrap_or("?"),
                    v["refinement"].as_str().unwrap_or(""),
                ))
                .collect::<Vec<_>>()
                .join("\n");

            let hours_left = (effective_expiry - age).num_hours();
            let title = idea["title"].as_str().unwrap_or("(untitled)");
            let project = idea["project_id"].as_str().unwrap_or("?");

            let _ = state.bus_tx.send(json!({
                "type": "rocky:ask_human",
                "to": "rocky",
                "subject": "Idea expiring soon",
                "body": format!(
                    "The idea '{}' (project: {}) expires in ~{}h with {} votes.\n\nShould I extend it, promote it anyway, or let it expire?\n\nCurrent votes:\n{}",
                    title, project, hours_left, votes.len(), vote_summary
                ),
                "idea_task_id": task_id,
                "actions": ["extend_7d", "promote_anyway", "let_expire"],
            }).to_string());

            // Mark warned in metadata
            update_task_meta_field(state, task_id, "expiry_warned", json!(true)).await;
            update_task_meta_field(state, task_id, "rocky_warn_sent_at", json!(now.to_rfc3339())).await;
            info!("[dispatch] rocky escalation sent for idea={}", task_id);
        }
    }
}

// ── Idle discovery ────────────────────────────────────────────────────────────

async fn detect_and_assign_discovery(
    state: &Arc<AppState>,
    cfg: &DispatchConfig,
    agents: &Value,
    claimed_counts: &HashMap<String, usize>,
    now: DateTime<Utc>,
) {
    let idle_agents = detect_idle_agents(state, cfg, agents, claimed_counts, now).await;

    for agent_name in &idle_agents {
        maybe_create_discovery_task(state, agent_name, now).await;
    }
}

/// Returns names of agents that are online, past grace period, have no active tasks,
/// and have no open non-discovery/non-idea tasks they could claim.
pub async fn detect_idle_agents(
    state: &Arc<AppState>,
    cfg: &DispatchConfig,
    agents: &Value,
    claimed_counts: &HashMap<String, usize>,
    now: DateTime<Utc>,
) -> Vec<String> {
    let open_real_tasks = fetch_open_dispatchable_tasks(state).await;

    let mut idle = Vec::new();

    if let Some(obj) = agents.as_object() {
        for (name, agent) in obj {
            if !is_agent_online(agent) { continue; }

            // Grace period check
            let online_since = agent.get("online_since")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            if let Some(since) = online_since {
                if (now - since).num_seconds() < cfg.idle_grace_period_secs { continue; }
            }

            // No active tasks
            if claimed_counts.get(name).copied().unwrap_or(0) > 0 { continue; }

            // No claimable real work
            let max_per_agent = cfg.max_tasks_per_agent;
            let has_work = open_real_tasks.iter().any(|task| {
                select_best_agent(task, agents, claimed_counts, &[], max_per_agent).as_deref() == Some(name.as_str())
                || task["metadata"]["preferred_executor"].as_str().is_none()
            });
            if has_work { continue; }

            idle.push(name.clone());
        }
    }
    idle
}

async fn maybe_create_discovery_task(
    state: &Arc<AppState>,
    agent_name: &str,
    now: DateTime<Utc>,
) {
    // Already has a discovery task?
    let existing = {
        let db = state.fleet_db.lock().await;
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM fleet_tasks WHERE claimed_by=?1 AND task_type='discovery' AND status IN ('claimed','in_progress','open')",
            params![agent_name],
            |r| r.get(0),
        ).unwrap_or(0);
        count > 0
    };
    if existing { return; }

    // Find project with no open idea tasks, oldest activity first
    let project_id = {
        let db = state.fleet_db.lock().await;
        let mut stmt = db.prepare(
            "SELECT DISTINCT project_id FROM fleet_tasks \
             WHERE project_id NOT IN (SELECT DISTINCT project_id FROM fleet_tasks WHERE task_type='idea' AND status='open') \
             ORDER BY (SELECT MAX(updated_at) FROM fleet_tasks t2 WHERE t2.project_id=fleet_tasks.project_id) ASC \
             LIMIT 1"
        ).ok();
        stmt.as_mut().and_then(|s| {
            s.query_row([], |r| r.get::<_, String>(0)).ok()
        })
    };

    let project_id = match project_id {
        Some(p) => p,
        None => return, // all projects have ideas
    };

    let task_id = format!("task-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let now_str = now.to_rfc3339();
    let expires_str = (now + Duration::hours(4)).to_rfc3339();
    let title = format!("Explore {} and propose improvement ideas", project_id);
    let description = format!(
        "You have no assigned work. Explore the {} project's codebase, recent tasks, and open issues. \
         Identify gaps, inefficiencies, or missing capabilities. For each meaningful finding, \
         create an idea task using POST /api/tasks with task_type=idea.",
        project_id
    );
    let meta = json!({ "auto_created": true, "trigger": "idle" }).to_string();

    {
        let db = state.fleet_db.lock().await;
        let _ = db.execute(
            "INSERT INTO fleet_tasks (id,project_id,title,description,priority,status,claimed_by,claimed_at,claim_expires_at,metadata,task_type,phase,blocked_by) \
             VALUES (?1,?2,?3,?4,4,'claimed',?5,?6,?7,?8,'discovery','build','[]')",
            params![task_id, project_id, title, description, agent_name, now_str, expires_str, meta],
        );
    }

    let _ = state.bus_tx.send(json!({
        "type": "tasks:dispatch_assigned",
        "to": agent_name,
        "task_id": task_id,
    }).to_string());
    info!("[dispatch] discovery task created task={} agent={} project={}", task_id, agent_name, project_id);
}

// ── DB helpers ────────────────────────────────────────────────────────────────

async fn get_claimed_counts(state: &Arc<AppState>) -> HashMap<String, usize> {
    let db = state.fleet_db.lock().await;
    let mut stmt = match db.prepare(
        "SELECT claimed_by, COUNT(*) FROM fleet_tasks \
         WHERE status IN ('claimed','in_progress') AND claimed_by IS NOT NULL \
         GROUP BY claimed_by"
    ) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    stmt.query_map([], |r| {
        let name: String = r.get(0)?;
        let count: i64 = r.get(1)?;
        Ok((name, count as usize))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

async fn fetch_open_dispatchable_tasks(state: &Arc<AppState>) -> Vec<Value> {
    let db = state.fleet_db.lock().await;
    let mut stmt = match db.prepare(
        "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,\
         claim_expires_at,completed_at,completed_by,created_at,metadata,\
         task_type,review_of,phase,blocked_by,review_result \
         FROM fleet_tasks \
         WHERE status='open' AND task_type NOT IN ('discovery','idea') \
         ORDER BY priority ASC, created_at ASC"
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map([], row_to_value)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

async fn fetch_open_ideas(state: &Arc<AppState>) -> Vec<Value> {
    let db = state.fleet_db.lock().await;
    let mut stmt = match db.prepare(
        "SELECT id,project_id,title,description,status,priority,claimed_by,claimed_at,\
         claim_expires_at,completed_at,completed_by,created_at,metadata,\
         task_type,review_of,phase,blocked_by,review_result \
         FROM fleet_tasks \
         WHERE status='open' AND task_type='idea' \
         ORDER BY created_at ASC"
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map([], row_to_value)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

fn row_to_value(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let metadata_str: String = row.get(12)?;
    let metadata: Value = serde_json::from_str(&metadata_str).unwrap_or(json!({}));
    let blocked_by_str: String = row.get(16).unwrap_or_else(|_| "[]".to_string());
    let blocked_by: Value = serde_json::from_str(&blocked_by_str).unwrap_or(json!([]));
    Ok(json!({
        "id":               row.get::<_, String>(0)?,
        "project_id":       row.get::<_, String>(1)?,
        "title":            row.get::<_, String>(2)?,
        "description":      row.get::<_, String>(3)?,
        "status":           row.get::<_, String>(4)?,
        "priority":         row.get::<_, i64>(5)?,
        "claimed_by":       row.get::<_, Option<String>>(6)?,
        "claimed_at":       row.get::<_, Option<String>>(7)?,
        "claim_expires_at": row.get::<_, Option<String>>(8)?,
        "completed_at":     row.get::<_, Option<String>>(9)?,
        "completed_by":     row.get::<_, Option<String>>(10)?,
        "created_at":       row.get::<_, String>(11)?,
        "metadata":         metadata,
        "task_type":        row.get::<_, String>(13).unwrap_or_else(|_| "work".to_string()),
        "review_of":        row.get::<_, Option<String>>(14)?,
        "phase":            row.get::<_, Option<String>>(15)?,
        "blocked_by":       blocked_by,
        "review_result":    row.get::<_, Option<String>>(17)?,
    }))
}

async fn update_nudge_meta(state: &Arc<AppState>, task_id: &str, task: &Value, now: DateTime<Utc>) {
    let raw_meta = task["metadata"].clone();
    let mut meta: Value = if raw_meta.is_object() { raw_meta } else { json!({}) };
    let nudge_count = meta["dispatch"]["nudge_count"].as_i64().unwrap_or(0) + 1;
    meta["dispatch"]["nudge_count"] = json!(nudge_count);
    meta["dispatch"]["last_nudge_at"] = json!(now.to_rfc3339());
    let db = state.fleet_db.lock().await;
    let _ = db.execute(
        "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
        params![meta.to_string(), now.to_rfc3339(), task_id],
    );
}

async fn update_assign_meta(state: &Arc<AppState>, task_id: &str) {
    let now = Utc::now().to_rfc3339();
    let db = state.fleet_db.lock().await;
    let raw: String = db.query_row(
        "SELECT metadata FROM fleet_tasks WHERE id=?1", params![task_id], |r| r.get(0)
    ).unwrap_or_else(|_| "{}".to_string());
    let mut meta: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
    let attempts = meta["dispatch"]["assign_attempts"].as_i64().unwrap_or(0) + 1;
    meta["dispatch"]["assign_attempts"] = json!(attempts);
    meta["dispatch"]["last_assign_at"] = json!(now);
    let _ = db.execute(
        "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
        params![meta.to_string(), now, task_id],
    );
}

async fn update_task_meta_field(state: &Arc<AppState>, task_id: &str, field: &str, value: Value) {
    let db = state.fleet_db.lock().await;
    let raw: String = db.query_row(
        "SELECT metadata FROM fleet_tasks WHERE id=?1", params![task_id], |r| r.get(0)
    ).unwrap_or_else(|_| "{}".to_string());
    let mut meta: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
    meta[field] = value;
    let now = Utc::now().to_rfc3339();
    let _ = db.execute(
        "UPDATE fleet_tasks SET metadata=?1, updated_at=?2 WHERE id=?3",
        params![meta.to_string(), now, task_id],
    );
}

// ── Unit tests (pure functions — no I/O) ──────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_agents(entries: &[(&str, bool, &[&str], i64)]) -> Value {
        // entries: (name, online, capabilities[], seconds_ago_seen)
        let mut map = serde_json::Map::new();
        for (name, online, caps, secs_ago) in entries {
            let last_seen = if *online {
                (Utc::now() - Duration::seconds(*secs_ago)).to_rfc3339()
            } else {
                (Utc::now() - Duration::seconds(400)).to_rfc3339()
            };
            let mut caps_map = serde_json::Map::new();
            for cap in *caps {
                caps_map.insert(cap.to_string(), json!(true));
            }
            map.insert(name.to_string(), json!({
                "lastSeen": last_seen,
                "capabilities": caps_map,
            }));
        }
        Value::Object(map)
    }

    fn make_task(preferred_executor: Option<&str>) -> Value {
        let mut meta = json!({});
        if let Some(exec) = preferred_executor {
            meta["preferred_executor"] = json!(exec);
        }
        json!({ "id": "task-1", "metadata": meta })
    }

    #[test]
    fn test_capability_match_no_requirement() {
        let agents = make_agents(&[
            ("alpha", true, &[], 10),
            ("beta",  true, &["gpu"], 10),
        ]);
        let task = make_task(None);
        let result = select_best_agent(&task, &agents, &HashMap::new(), &[], 99);
        assert!(result.is_some());
    }

    #[test]
    fn test_capability_match_specific_executor() {
        let agents = make_agents(&[
            ("cpu-agent", true, &["claude_cli"], 10),
            ("gpu-agent", true, &["gpu", "claude_cli"], 10),
        ]);
        let task = make_task(Some("gpu"));
        let result = select_best_agent(&task, &agents, &HashMap::new(), &[], 99);
        assert_eq!(result.as_deref(), Some("gpu-agent"));
    }

    #[test]
    fn test_offline_agent_excluded() {
        let agents = make_agents(&[
            ("online",  true,  &[], 10),
            ("offline", false, &[], 10),
        ]);
        let task = make_task(None);
        let result = select_best_agent(&task, &agents, &HashMap::new(), &[], 99);
        assert_eq!(result.as_deref(), Some("online"));
    }

    #[test]
    fn test_select_least_loaded_agent() {
        let agents = make_agents(&[
            ("busy",  true, &[], 10),
            ("light", true, &[], 10),
        ]);
        let mut counts = HashMap::new();
        counts.insert("busy".to_string(), 3);
        counts.insert("light".to_string(), 1);
        let task = make_task(None);
        let result = select_best_agent(&task, &agents, &counts, &[], 99);
        assert_eq!(result.as_deref(), Some("light"));
    }

    #[test]
    fn test_tiebreak_alphabetical() {
        let agents = make_agents(&[
            ("zebra", true, &[], 10),
            ("alpha", true, &[], 10),
            ("mango", true, &[], 10),
        ]);
        let task = make_task(None);
        let result = select_best_agent(&task, &agents, &HashMap::new(), &[], 99);
        assert_eq!(result.as_deref(), Some("alpha"));
    }

    #[test]
    fn test_no_eligible_agents_returns_none() {
        let agents = make_agents(&[
            ("offline", false, &[], 10),
        ]);
        let task = make_task(None);
        let result = select_best_agent(&task, &agents, &HashMap::new(), &[], 99);
        assert!(result.is_none());
    }

    #[test]
    fn test_blacklisted_agent_excluded() {
        let agents = make_agents(&[
            ("agent-a", true, &[], 10),
            ("agent-b", true, &[], 10),
        ]);
        let task = make_task(None);
        let blacklist = vec!["agent-a".to_string()];
        let result = select_best_agent(&task, &agents, &HashMap::new(), &blacklist, 99);
        assert_eq!(result.as_deref(), Some("agent-b"));
    }

    #[test]
    fn test_all_blacklisted_returns_none() {
        let agents = make_agents(&[
            ("agent-a", true, &[], 10),
        ]);
        let task = make_task(None);
        let blacklist = vec!["agent-a".to_string()];
        let result = select_best_agent(&task, &agents, &HashMap::new(), &blacklist, 99);
        assert!(result.is_none());
    }

    #[test]
    fn test_is_agent_online_recent() {
        let agent = json!({ "lastSeen": (Utc::now() - Duration::seconds(10)).to_rfc3339() });
        assert!(is_agent_online(&agent));
    }

    #[test]
    fn test_is_agent_online_stale() {
        let agent = json!({ "lastSeen": (Utc::now() - Duration::seconds(400)).to_rfc3339() });
        assert!(!is_agent_online(&agent));
    }

    #[test]
    fn test_is_agent_online_no_lastseen() {
        let agent = json!({});
        assert!(!is_agent_online(&agent));
    }
}
