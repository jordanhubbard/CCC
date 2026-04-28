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

use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio::sync::watch;

use crate::config::Config;
use crate::upgrade::UpgradeOptions;

const HEALTHY_UPTIME: Duration = Duration::from_secs(300);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

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

fn nvidia_enabled(_: &Config) -> bool {
    std::env::var("NVIDIA_API_BASE").is_ok()
}

fn slack_gateway_enabled(_: &Config) -> bool {
    std::env::var("SLACK_APP_TOKEN")
        .map(|t| t.starts_with("xapp-"))
        .unwrap_or(false)
}

fn slack_gateway_ofterra_enabled(_: &Config) -> bool {
    std::env::var("SLACK_APP_TOKEN_OFTERRA")
        .map(|t| t.starts_with("xapp-"))
        .unwrap_or(false)
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
        enabled: always,
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
        enabled: always,
    },
    ChildSpec {
        name: "gateway",
        args: &["hermes", "--gateway"],
        direct_exe: false,
        enabled: slack_gateway_enabled,
    },
    ChildSpec {
        name: "gateway-ofterra",
        args: &["hermes", "--gateway", "--workspace", "ofterra"],
        direct_exe: false,
        enabled: slack_gateway_ofterra_enabled,
    },
    ChildSpec {
        name: "proxy",
        args: &["proxy", "--port", "9099"],
        direct_exe: false,
        enabled: nvidia_enabled,
    },
];

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

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut join_handles = Vec::new();
    for (name, child_exe, child_args, _direct) in processes {
        let child_name = name;
        let rx = shutdown_rx.clone();
        let agent_name = cfg.agent_name.clone();
        let acc_dir = cfg.acc_dir.clone();

        join_handles.push(tokio::spawn(async move {
            child_loop(child_exe, child_name, child_args, rx, agent_name, acc_dir).await;
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
) {
    let mut backoff = Duration::from_secs(1);

    loop {
        if *shutdown.borrow() {
            break;
        }

        let started = Instant::now();
        child_log(&agent_name, &acc_dir, &name, "starting");

        let mut child = match tokio::process::Command::new(&exe).args(&args).spawn() {
            Ok(c) => c,
            Err(e) => {
                child_log(&agent_name, &acc_dir, &name, &format!("spawn failed: {e}"));
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
            graceful_kill(&mut child).await;
            break;
        }

        let uptime = started.elapsed();
        match exit_status.unwrap() {
            Ok(s) => child_log(
                &agent_name,
                &acc_dir,
                &name,
                &format!("exited {s} (uptime {uptime:?})"),
            ),
            Err(e) => child_log(&agent_name, &acc_dir, &name, &format!("wait error: {e}")),
        }

        if uptime >= HEALTHY_UPTIME {
            backoff = Duration::from_secs(1);
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
