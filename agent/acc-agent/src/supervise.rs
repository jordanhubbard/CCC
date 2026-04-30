//! Single-process supervisor for all ACC agent children.
//!
//! `acc-agent supervise` is the only binary that needs a systemd unit or launchd
//! plist. It owns the full child lifecycle: spawn, watch, restart with backoff.
//!
//! Worker set is determined at startup:
//!   1. Read `~/.acc/acc.json` → `supervisor.processes` if present.
//!   2. Fall back to the compiled-in CHILDREN list otherwise.
//!   Both paths apply the same per-worker `enabled` predicates.
//!
//! Signals:
//!   SIGTERM / SIGINT  → graceful shutdown (SIGTERM each child, wait, then exit 0)
//!   SIGUSR1           → graceful restart  (same stop sequence, then re-exec self)
//!
//! Signal handlers are installed BEFORE run_upgrade so that any SIGUSR1 sent during
//! the initial migration pass is buffered by the kernel and handled after upgrade
//! returns, rather than hitting the default (terminate) action.

use acc_client::Client;
use acc_model::HeartbeatRequest;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio::sync::watch;

use crate::config::Config;
use crate::session_registry;
use crate::upgrade::UpgradeOptions;

const HEALTHY_UPTIME: Duration = Duration::from_secs(300);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
const SUPERVISOR_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

type ChildHealthMap = Arc<Mutex<BTreeMap<String, ChildHealth>>>;

#[derive(Debug, Clone, Serialize)]
struct ChildHealth {
    name: String,
    workspace: Option<String>,
    command: String,
    status: String,
    running: bool,
    pid: Option<u32>,
    restart_count: u64,
    last_started_at: Option<DateTime<Utc>>,
    last_exit_at: Option<DateTime<Utc>>,
    last_exit_status: Option<String>,
    last_uptime_ms: Option<u128>,
    next_restart_backoff_secs: Option<u64>,
    last_error: Option<String>,
}

impl ChildHealth {
    fn configured(name: &str, exe: &std::path::Path, args: &[String]) -> Self {
        Self {
            name: name.to_string(),
            workspace: gateway_workspace(name, args),
            command: std::iter::once(exe.display().to_string())
                .chain(args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
            status: "configured".to_string(),
            running: false,
            pid: None,
            restart_count: 0,
            last_started_at: None,
            last_exit_at: None,
            last_exit_status: None,
            last_uptime_ms: None,
            next_restart_backoff_secs: None,
            last_error: None,
        }
    }
}

// ── Static child spec (compile-time default set) ──────────────────────────────

struct ChildSpec {
    name: &'static str,
    args: &'static [&'static str],
    direct_exe: bool,
    enabled: fn(&Config) -> bool,
}

// ── Dynamic child spec (loaded from acc.json at runtime) ──────────────────────

/// One worker entry from `~/.acc/acc.json` → `supervisor.processes`.
#[derive(serde::Deserialize, Clone)]
struct DynProcess {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    /// Optional: only start this process if this env var is present.
    #[serde(default)]
    enabled_if_env: Option<String>,
}

/// Load `supervisor.processes` from `~/.acc/acc.json` if it exists.
/// Returns None if the file is absent, unreadable, or has no processes array.
fn load_acc_json_processes() -> Option<Vec<DynProcess>> {
    let path = std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".acc").join("acc.json"))
        .or_else(|| Some(PathBuf::from("/root/.acc/acc.json")))?;
    let text = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let procs = v.get("supervisor")?.get("processes")?;
    let list: Vec<DynProcess> = serde_json::from_value(procs.clone()).ok()?;
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

fn always(_: &Config) -> bool {
    true
}

fn env_flag(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

fn legacy_queue_enabled(_: &Config) -> bool {
    env_flag("ACC_ENABLE_LEGACY_QUEUE", false)
}

fn hermes_poll_enabled(_: &Config) -> bool {
    env_flag("ACC_ENABLE_HERMES_POLL", false) || env_flag("ACC_ENABLE_HERMES", false)
}

fn nvidia_enabled(_: &Config) -> bool {
    std::env::var("NVIDIA_API_BASE").is_ok()
}

fn slack_gateway_enabled(_: &Config) -> bool {
    std::env::var("SLACK_APP_TOKEN")
        .map(|t| t.starts_with("xapp-"))
        .unwrap_or(false)
}

fn slack_gateway_offtera_enabled(_: &Config) -> bool {
    // Accept both the corrected name and the historical typo so that hosts
    // mid-migration with the old env-var name still light up the gateway.
    let has = |k: &str| {
        std::env::var(k)
            .map(|t| t.starts_with("xapp-"))
            .unwrap_or(false)
    };
    has("SLACK_APP_TOKEN_OFFTERA") || has("SLACK_APP_TOKEN_OFTERRA")
}

fn slack_ingest_enabled(_: &Config) -> bool {
    // Run the Slack→Qdrant ingester on the hub only — that is where Qdrant
    // lives and where vault-stored bot tokens are reachable in-process at
    // sub-millisecond latency. Other hosts' agents read memory; only the
    // hub writes it.
    std::env::var("IS_HUB")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

fn gateway_workspace(name: &str, args: &[String]) -> Option<String> {
    if !name.starts_with("gateway") {
        return None;
    }
    args.windows(2)
        .find(|w| w[0] == "--workspace")
        .map(|w| w[1].to_lowercase())
        .or_else(|| Some("omgjkh".to_string()))
}

fn update_child_health<F>(health: &ChildHealthMap, child: &str, update: F)
where
    F: FnOnce(&mut ChildHealth),
{
    if let Ok(mut map) = health.lock() {
        if let Some(entry) = map.get_mut(child) {
            update(entry);
        }
    }
}

fn gateway_health_snapshot(health: Option<&ChildHealthMap>) -> Option<Value> {
    let health = health?;
    let map = health.lock().ok()?;
    let gateways: serde_json::Map<String, Value> = map
        .iter()
        .filter(|(name, entry)| name.starts_with("gateway") || entry.workspace.is_some())
        .filter_map(|(name, entry)| serde_json::to_value(entry).ok().map(|v| (name.clone(), v)))
        .collect();
    if gateways.is_empty() {
        None
    } else {
        Some(json!({
            "version": 1,
            "updated_at": Utc::now(),
            "children": gateways,
        }))
    }
}

static CHILDREN: &[ChildSpec] = &[
    ChildSpec {
        name: "bus",
        args: &["bus"],
        direct_exe: false,
        enabled: always,
    },
    ChildSpec {
        name: "queue",
        args: &["queue"],
        direct_exe: false,
        enabled: legacy_queue_enabled,
    },
    ChildSpec {
        name: "tasks",
        args: &["tasks"],
        direct_exe: false,
        enabled: always,
    },
    ChildSpec {
        name: "hermes",
        args: &["hermes", "--poll"],
        direct_exe: false,
        enabled: hermes_poll_enabled,
    },
    ChildSpec {
        name: "gateway",
        args: &["hermes", "--gateway"],
        direct_exe: false,
        enabled: slack_gateway_enabled,
    },
    ChildSpec {
        name: "gateway-offtera",
        args: &["hermes", "--gateway", "--workspace", "offtera"],
        direct_exe: false,
        enabled: slack_gateway_offtera_enabled,
    },
    ChildSpec {
        name: "slack-ingest",
        args: &["slack-ingest"],
        direct_exe: false,
        enabled: slack_ingest_enabled,
    },
    ChildSpec {
        name: "proxy",
        args: &["proxy", "--port", "9099"],
        direct_exe: false,
        enabled: nvidia_enabled,
    },
    ChildSpec {
        name: "nap",
        args: &["nap", "--schedule"],
        direct_exe: false,
        enabled: always,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn minimal_default_runtime_gates_legacy_queue_and_hermes_poll() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("ACC_ENABLE_LEGACY_QUEUE");
        std::env::remove_var("ACC_ENABLE_HERMES_POLL");
        std::env::remove_var("ACC_ENABLE_HERMES");
        let cfg = Config {
            acc_dir: std::env::temp_dir(),
            acc_url: "http://example.test".into(),
            acc_token: "tok".into(),
            agent_name: "agent".into(),
            agentbus_token: "bus".into(),
            pair_programming: true,
            host: "host".into(),
            ssh_user: "user".into(),
            ssh_host: "host".into(),
            ssh_port: 22,
        };

        assert!(!legacy_queue_enabled(&cfg));
        assert!(!hermes_poll_enabled(&cfg));

        std::env::set_var("ACC_ENABLE_LEGACY_QUEUE", "true");
        std::env::set_var("ACC_ENABLE_HERMES_POLL", "1");
        assert!(legacy_queue_enabled(&cfg));
        assert!(hermes_poll_enabled(&cfg));

        std::env::remove_var("ACC_ENABLE_LEGACY_QUEUE");
        std::env::remove_var("ACC_ENABLE_HERMES_POLL");
    }

    #[test]
    fn supervisor_heartbeat_base_request_reports_idle_capacity() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("AGENT_MAX_TASKS");
        std::env::set_var("ACC_MAX_TASKS_PER_AGENT", "3");
        let cfg = Config {
            acc_dir: std::env::temp_dir(),
            acc_url: "http://example.test".into(),
            acc_token: "tok".into(),
            agent_name: "agent".into(),
            agentbus_token: "bus".into(),
            pair_programming: true,
            host: "host".into(),
            ssh_user: "user".into(),
            ssh_host: "ssh.example.test".into(),
            ssh_port: 2222,
        };

        let gateway_health = json!({
            "version": 1,
            "children": {
                "gateway": {"status": "running", "running": true}
            }
        });
        let req = build_supervisor_heartbeat_request(&cfg, Some(gateway_health.clone()));

        assert_eq!(req.status.as_deref(), Some("ok"));
        assert_eq!(req.note.as_deref(), Some("supervisor idle"));
        assert_eq!(req.host.as_deref(), Some("host"));
        assert_eq!(req.ssh_user.as_deref(), Some("user"));
        assert_eq!(req.ssh_host.as_deref(), Some("ssh.example.test"));
        assert_eq!(req.ssh_port, Some(2222));
        assert_eq!(req.tasks_in_flight, Some(0));
        assert_eq!(req.estimated_free_slots, Some(3));
        assert!(req.ccc_version.is_none());
        assert!(req.executors.is_empty());
        assert!(req.sessions.is_empty());
        assert_eq!(req.gateway_health, Some(gateway_health));

        std::env::remove_var("ACC_MAX_TASKS_PER_AGENT");
    }
}

pub async fn run(args: &[String]) {
    let dry_run = args.iter().any(|a| a == "--dry-run");

    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[supervise] config error: {e}");
            std::process::exit(1);
        }
    };

    log(&cfg, &format!("starting (pid={})", std::process::id()));

    let pid_path = cfg.acc_dir.join("supervisor.pid");
    let _ = std::fs::write(&pid_path, format!("{}\n", std::process::id()));

    // FIX #6: Register signal handlers BEFORE run_upgrade so that any SIGUSR1 sent
    // by a migration (declaring "# Restarts: acc-bus-listener") is buffered by the
    // kernel rather than triggering the default terminate action.
    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut sigusr1 = signal(SignalKind::user_defined1()).expect("SIGUSR1 handler");

    // Run initial upgrade before spawning children
    log(&cfg, "running upgrade pass");
    crate::upgrade::run_upgrade(&cfg, UpgradeOptions { dry_run }).await;

    if dry_run {
        log(&cfg, "dry-run: not starting children");
        let _ = std::fs::remove_file(&pid_path);
        return;
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("acc-agent"));

    // Log capabilities from the Worker trait before spawning.
    let caps = crate::worker::enabled_capabilities();
    log(&cfg, &format!("capabilities: {}", caps.join(", ")));

    // Build the process list: prefer acc.json config, fall back to compiled-in CHILDREN.
    // Represented as (name, exe_path, args, direct_exe) tuples for uniform spawning.
    let processes: Vec<(String, PathBuf, Vec<String>, bool)> =
        if let Some(dyn_procs) = load_acc_json_processes() {
            log(
                &cfg,
                &format!("using acc.json worker config ({} entries)", dyn_procs.len()),
            );
            dyn_procs
                .into_iter()
                .filter(|p| {
                    // Per-process env var gate.
                    p.enabled_if_env.as_ref()
                        .map(|v| std::env::var(v).is_ok())
                        .unwrap_or(true)
                    // Also apply static enabled predicate if we know this worker.
                    && CHILDREN.iter()
                        .find(|c| c.name == p.name.as_str())
                        .map(|c| (c.enabled)(&cfg))
                        .unwrap_or(true)
                })
                .map(|p| {
                    let child_exe = if p.command == "acc-agent" {
                        exe.clone()
                    } else {
                        PathBuf::from(&p.command)
                    };
                    let direct = p.command != "acc-agent";
                    (p.name.clone(), child_exe, p.args.clone(), direct)
                })
                .collect()
        } else {
            CHILDREN
                .iter()
                .filter(|c| (c.enabled)(&cfg))
                .map(|c| {
                    let (child_exe, child_args) = if c.direct_exe {
                        (
                            PathBuf::from(c.args[0]),
                            c.args[1..].iter().map(|s| s.to_string()).collect(),
                        )
                    } else {
                        (exe.clone(), c.args.iter().map(|s| s.to_string()).collect())
                    };
                    (c.name.to_string(), child_exe, child_args, c.direct_exe)
                })
                .collect()
        };

    log(
        &cfg,
        &format!(
            "spawning: {}",
            processes
                .iter()
                .map(|(n, _, _, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );

    let child_health: ChildHealthMap = Arc::new(Mutex::new(
        processes
            .iter()
            .map(|(name, child_exe, child_args, _)| {
                (
                    name.clone(),
                    ChildHealth::configured(name, child_exe, child_args),
                )
            })
            .collect(),
    ));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut join_handles = Vec::new();
    join_handles.push(spawn_supervisor_heartbeat(
        cfg.clone(),
        child_health.clone(),
        shutdown_rx.clone(),
    ));
    for (name, child_exe, child_args, _direct) in processes {
        let child_name = name;
        let rx = shutdown_rx.clone();
        let agent_name = cfg.agent_name.clone();
        let acc_dir = cfg.acc_dir.clone();
        let health = child_health.clone();

        join_handles.push(tokio::spawn(async move {
            child_loop(
                child_exe, child_name, child_args, rx, agent_name, acc_dir, health,
            )
            .await;
        }));
    }
    // Await a signal (handlers were already registered above)
    let do_restart = await_signal(&mut sigterm, &mut sigint, &mut sigusr1).await;

    // Broadcast shutdown to all child loops
    let _ = shutdown_tx.send(true);

    // Give children time to notice and exit gracefully
    let _ = tokio::time::timeout(
        SHUTDOWN_GRACE + Duration::from_secs(2),
        futures_util::future::join_all(join_handles),
    )
    .await;

    if do_restart {
        // FIX #7: Do NOT delete pid_path before exec(). exec() preserves the PID,
        // so the new process image will overwrite it with the same value. Deleting
        // first creates a window where upgrade.rs falls through to the spawn fallback.
        log(&cfg, "re-execing self");
        re_exec_self(&exe);
    }

    let _ = std::fs::remove_file(&pid_path);
    log(&cfg, "stopped");
}

fn spawn_supervisor_heartbeat(
    cfg: Config,
    health: ChildHealthMap,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let client = match Client::new(cfg.acc_url.clone(), &cfg.acc_token) {
            Ok(client) => client,
            Err(err) => {
                log(
                    &cfg,
                    &format!("WARNING: heartbeat client setup failed: {err}"),
                );
                return;
            }
        };

        log(&cfg, "supervisor heartbeat started");
        post_supervisor_heartbeat(&cfg, &client, &health).await;

        let mut interval = tokio::time::interval(SUPERVISOR_HEARTBEAT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    post_supervisor_heartbeat(&cfg, &client, &health).await;
                }
                _ = shutdown.changed() => break,
            }
        }

        log(&cfg, "supervisor heartbeat stopped");
    })
}

async fn post_supervisor_heartbeat(cfg: &Config, client: &Client, health: &ChildHealthMap) {
    let mut req = build_supervisor_heartbeat_request(cfg, gateway_health_snapshot(Some(health)));
    session_registry::augment_heartbeat(cfg, &mut req).await;

    match tokio::time::timeout(
        Duration::from_secs(15),
        client.items().heartbeat(&cfg.agent_name, &req),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => log(cfg, &format!("WARNING: supervisor heartbeat failed: {err}")),
        Err(_) => log(cfg, "WARNING: supervisor heartbeat timed out"),
    }
}

fn build_supervisor_heartbeat_request(
    cfg: &Config,
    gateway_health: Option<Value>,
) -> HeartbeatRequest {
    HeartbeatRequest {
        ts: Some(chrono::Utc::now()),
        status: Some("ok".into()),
        note: Some("supervisor idle".into()),
        host: Some(cfg.host.clone()),
        ssh_user: Some(cfg.ssh_user.clone()),
        ssh_host: Some(cfg.ssh_host.clone()),
        ssh_port: Some(cfg.ssh_port as u64),
        tasks_in_flight: Some(0),
        estimated_free_slots: Some(cfg.max_tasks_per_agent()),
        free_session_slots: None,
        max_sessions: None,
        session_spawn_denied_reason: None,
        ccc_version: None,
        workspace_revision: None,
        runtime_version: None,
        executors: vec![],
        sessions: vec![],
        gateway_health,
    }
}

async fn await_signal(sigterm: &mut Signal, sigint: &mut Signal, sigusr1: &mut Signal) -> bool {
    tokio::select! {
        _ = sigterm.recv() => { eprintln!("[supervise] SIGTERM — shutting down"); false }
        _ = sigint.recv()  => { eprintln!("[supervise] SIGINT — shutting down");  false }
        _ = sigusr1.recv() => { eprintln!("[supervise] SIGUSR1 — restarting");    true  }
    }
}

async fn child_loop(
    exe: PathBuf,
    name: String,
    args: Vec<String>,
    mut shutdown: watch::Receiver<bool>,
    agent_name: String,
    acc_dir: PathBuf,
    health: ChildHealthMap,
) {
    let mut backoff = Duration::from_secs(1);

    loop {
        if *shutdown.borrow() {
            break;
        }

        let started = Instant::now();
        child_log(&agent_name, &acc_dir, &name, "starting");
        update_child_health(&health, &name, |entry| {
            entry.status = "starting".to_string();
            entry.running = false;
            entry.pid = None;
            entry.last_started_at = Some(Utc::now());
            entry.next_restart_backoff_secs = None;
            entry.last_error = None;
        });

        let mut child = match tokio::process::Command::new(&exe).args(&args).spawn() {
            Ok(c) => {
                let pid = c.id();
                update_child_health(&health, &name, |entry| {
                    entry.status = "running".to_string();
                    entry.running = true;
                    entry.pid = pid;
                });
                c
            }
            Err(e) => {
                child_log(&agent_name, &acc_dir, &name, &format!("spawn failed: {e}"));
                update_child_health(&health, &name, |entry| {
                    entry.status = "spawn_failed".to_string();
                    entry.running = false;
                    entry.pid = None;
                    entry.last_error = Some(e.to_string());
                    entry.next_restart_backoff_secs = Some(backoff.as_secs());
                });
                backoff = sleep_or_shutdown(backoff, &mut shutdown).await;
                if *shutdown.borrow() {
                    break;
                }
                continue;
            }
        };

        // Wait for child exit or shutdown signal
        let exit_status = tokio::select! {
            s = child.wait() => Some(s),
            _ = shutdown.changed() => None,
        };

        if exit_status.is_none() {
            child_log(&agent_name, &acc_dir, &name, "stopping on shutdown signal");
            update_child_health(&health, &name, |entry| {
                entry.status = "stopping".to_string();
                entry.running = false;
                entry.pid = None;
                entry.next_restart_backoff_secs = None;
            });
            graceful_kill(&mut child).await;
            break;
        }

        let uptime = started.elapsed();
        let exit_label = match exit_status.as_ref().unwrap() {
            Ok(s) => s.to_string(),
            Err(e) => e.to_string(),
        };
        match exit_status.unwrap() {
            Ok(s) => child_log(
                &agent_name,
                &acc_dir,
                &name,
                &format!("exited {s} (uptime {uptime:?})"),
            ),
            Err(e) => child_log(&agent_name, &acc_dir, &name, &format!("wait error: {e}")),
        }
        update_child_health(&health, &name, |entry| {
            entry.status = "restarting".to_string();
            entry.running = false;
            entry.pid = None;
            entry.restart_count = entry.restart_count.saturating_add(1);
            entry.last_exit_at = Some(Utc::now());
            entry.last_exit_status = Some(exit_label);
            entry.last_uptime_ms = Some(uptime.as_millis());
            entry.next_restart_backoff_secs = Some(backoff.as_secs());
        });

        if uptime >= HEALTHY_UPTIME {
            backoff = Duration::from_secs(1);
            update_child_health(&health, &name, |entry| {
                entry.next_restart_backoff_secs = Some(backoff.as_secs());
            });
        }

        child_log(
            &agent_name,
            &acc_dir,
            &name,
            &format!("restarting in {backoff:?}"),
        );
        backoff = sleep_or_shutdown(backoff, &mut shutdown).await;
        if *shutdown.borrow() {
            break;
        }
    }

    update_child_health(&health, &name, |entry| {
        entry.status = "stopped".to_string();
        entry.running = false;
        entry.pid = None;
        entry.next_restart_backoff_secs = None;
    });
    child_log(&agent_name, &acc_dir, &name, "stopped");
}

/// Sleep for `current` duration, watching for shutdown. Returns next backoff value.
async fn sleep_or_shutdown(current: Duration, shutdown: &mut watch::Receiver<bool>) -> Duration {
    tokio::select! {
        _ = tokio::time::sleep(current) => {}
        _ = shutdown.changed() => {}
    }
    (current * 2).min(MAX_BACKOFF)
}

/// SIGTERM the child, wait up to SHUTDOWN_GRACE, then SIGKILL.
async fn graceful_kill(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("kill")
            .args(["-15", &pid.to_string()])
            .status();
        if tokio::time::timeout(SHUTDOWN_GRACE, child.wait())
            .await
            .is_err()
        {
            let _ = child.start_kill();
            // Reap after SIGKILL so we don't accumulate zombies across re-execs
            let _ = child.wait().await;
        }
    } else {
        let _ = child.start_kill();
    }
}

fn re_exec_self(exe: &PathBuf) -> ! {
    use std::os::unix::process::CommandExt;
    let args: Vec<String> = std::env::args().collect();
    let err = std::process::Command::new(exe).args(&args[1..]).exec();
    eprintln!("[supervise] re-exec failed: {err}");
    std::process::exit(1);
}

fn log(cfg: &Config, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{}] [supervise] {msg}", cfg.agent_name);
    eprintln!("{line}");
    let path = cfg.acc_dir.join("logs").join("supervise.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
    tracing::info!(component = "supervise", agent = %cfg.agent_name, "{msg}");
}

fn child_log(agent_name: &str, acc_dir: &std::path::Path, child: &str, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{agent_name}] [supervise/{child}] {msg}");
    eprintln!("{line}");
    let path = acc_dir.join("logs").join("supervise.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}
