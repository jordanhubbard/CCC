//! Single-process supervisor for all ACC agent children.
//!
//! `acc-agent supervise` is the only binary that needs a systemd unit or launchd
//! plist. It owns the full child lifecycle: spawn, watch, restart with backoff.
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

struct ChildSpec {
    name: &'static str,
    /// For acc-agent subcommands: args[0] is the subcommand name.
    /// For direct executables (direct_exe = true): args[0] is the binary path.
    args: &'static [&'static str],
    /// If true, spawn args[0] as the executable directly (not as a subcommand).
    direct_exe: bool,
    enabled: fn(&Config) -> bool,
}

fn always(_: &Config) -> bool {
    true
}

fn nvidia_enabled(_: &Config) -> bool {
    std::env::var("NVIDIA_API_BASE").is_ok()
}

fn hermes_enabled(_: &Config) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::Path::new(&format!("{home}/.local/bin/hermes")).exists()
}

fn acc_server_enabled(_: &Config) -> bool {
    std::path::Path::new("/usr/local/bin/acc-server").exists()
        || std::path::Path::new("/usr/bin/acc-server").exists()
}

static CHILDREN: &[ChildSpec] = &[
    ChildSpec { name: "bus",        args: &["bus"],                           direct_exe: false, enabled: always            },
    ChildSpec { name: "queue",      args: &["queue"],                         direct_exe: false, enabled: always            },
    ChildSpec { name: "tasks",      args: &["tasks"],                         direct_exe: false, enabled: always            },
    ChildSpec { name: "hermes",     args: &["hermes", "--poll"],              direct_exe: false, enabled: hermes_enabled    },
    ChildSpec { name: "proxy",      args: &["proxy", "--port", "9099"],       direct_exe: false, enabled: nvidia_enabled    },
    ChildSpec { name: "acc-server", args: &["/usr/local/bin/acc-server"],     direct_exe: true,  enabled: acc_server_enabled},
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
    let mut sigint  = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut sigusr1 = signal(SignalKind::user_defined1()).expect("SIGUSR1 handler");

    // Run initial upgrade before spawning children
    log(&cfg, "running upgrade pass");
    crate::upgrade::run_upgrade(&cfg, UpgradeOptions { dry_run }).await;

    if dry_run {
        log(&cfg, "dry-run: not starting children");
        let _ = std::fs::remove_file(&pid_path);
        return;
    }

    let exe = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("acc-agent"));

    let enabled: Vec<&ChildSpec> = CHILDREN.iter()
        .filter(|c| (c.enabled)(&cfg))
        .collect();

    log(&cfg, &format!("spawning: {}",
        enabled.iter().map(|c| c.name).collect::<Vec<_>>().join(", ")));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut join_handles = Vec::new();
    for spec in &enabled {
        let (child_exe, child_args) = if spec.direct_exe {
            (
                PathBuf::from(spec.args[0]),
                spec.args[1..].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
        } else {
            (
                exe.clone(),
                spec.args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            )
        };

        let child_name = spec.name.to_string();
        let rx         = shutdown_rx.clone();
        let agent_name = cfg.agent_name.clone();
        let acc_dir    = cfg.acc_dir.clone();

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
    ).await;

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
                if *shutdown.borrow() { break; }
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
            Ok(s) => child_log(&agent_name, &acc_dir, &name,
                &format!("exited {s} (uptime {uptime:?})")),
            Err(e) => child_log(&agent_name, &acc_dir, &name,
                &format!("wait error: {e}")),
        }

        if uptime >= HEALTHY_UPTIME {
            backoff = Duration::from_secs(1);
        }

        child_log(&agent_name, &acc_dir, &name, &format!("restarting in {backoff:?}"));
        backoff = sleep_or_shutdown(backoff, &mut shutdown).await;
        if *shutdown.borrow() { break; }
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
        if tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await.is_err() {
            let _ = child.start_kill();
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
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

fn child_log(agent_name: &str, acc_dir: &std::path::Path, child: &str, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{agent_name}] [supervise/{child}] {msg}");
    eprintln!("{line}");
    let path = acc_dir.join("logs").join("supervise.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}
