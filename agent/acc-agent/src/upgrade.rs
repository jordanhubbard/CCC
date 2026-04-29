//! Upgrade orchestrator: run pending migrations, collect restart targets,
//! restart affected services in order (acc-server last), post heartbeat.
//!
//! Called by:
//!   - `acc-agent upgrade` CLI (manual runs / testing)
//!   - `bus::handle_update` after a successful inline git pull

use std::collections::HashSet;
use std::time::Duration;

use crate::config::Config;
use crate::migrate;
use crate::services;

pub struct UpgradeOptions {
    pub dry_run: bool,
}

/// Entry point from `main.rs`: `acc-agent upgrade [--dry-run]`
pub async fn run_cli(args: &[String]) {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[upgrade] config error: {e}");
            std::process::exit(1);
        }
    };
    run_upgrade(&cfg, UpgradeOptions { dry_run }).await;
}

/// Full upgrade lifecycle: migrations → service restarts → heartbeat → optional re-exec.
pub async fn run_upgrade(cfg: &Config, opts: UpgradeOptions) {
    let workspace = cfg.acc_dir.join("workspace");
    let migrations_dir = workspace.join("deploy/migrations");
    let state_path = cfg.acc_dir.join("migrations.json");

    log(
        cfg,
        &format!(
            "upgrade starting (dry_run={}, migrations={})",
            opts.dry_run,
            migrations_dir.display()
        ),
    );

    // ── 1. Load migration state ───────────────────────────────────────────────
    let mut state = migrate::load(&state_path);

    // ── 2. Collect pending migration scripts ──────────────────────────────────
    let scripts: Vec<_> = match std::fs::read_dir(&migrations_dir) {
        Ok(entries) => {
            let mut v: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".sh"))
                .collect();
            v.sort();
            v
        }
        Err(e) => {
            log(
                cfg,
                &format!("migrations dir unreadable: {e} — skipping migrations"),
            );
            vec![]
        }
    };

    // ── 3. Run pending migrations, collect restart targets ────────────────────
    let mut restart_names: HashSet<String> = HashSet::new();

    for script_file in &scripts {
        let script_name = script_file.trim_end_matches(".sh").to_string();

        // Skip migrations that have already been attempted (ok or failed).
        // Re-running failed migrations causes infinite hangs for environment-specific
        // failures (e.g. calico holding a port, consul not installed on macOS).
        // To force a retry, manually remove the entry from migrations.json.
        if state.contains_key(&script_name) {
            continue;
        }

        let script_path = migrations_dir.join(script_file);

        log(cfg, &format!("running migration: {script_name}"));

        if opts.dry_run {
            log(
                cfg,
                &format!("  [dry-run] would run: bash {}", script_path.display()),
            );
            // Still collect any declared restarts so dry-run output is informative
            if let Ok(contents) = std::fs::read_to_string(&script_path) {
                for svc in parse_restarts_header(&contents) {
                    log(cfg, &format!("  [dry-run] would restart: {svc}"));
                    restart_names.insert(svc);
                }
            }
            continue;
        }

        // Run the script
        let status = tokio::process::Command::new("bash")
            .arg(&script_path)
            .status()
            .await;

        match status {
            Ok(s) if s.success() => {
                log(cfg, &format!("migration {script_name}: ok"));
                state.insert(
                    script_name.clone(),
                    migrate::Record {
                        status: "ok".into(),
                        applied_at: chrono::Utc::now().to_rfc3339(),
                    },
                );
                migrate::save(&state_path, &state);

                // Parse # Restarts: header from the script source
                if let Ok(contents) = std::fs::read_to_string(&script_path) {
                    for svc in parse_restarts_header(&contents) {
                        log(cfg, &format!("  restart declared: {svc}"));
                        restart_names.insert(svc);
                    }
                }
            }
            Ok(s) => {
                log(cfg, &format!("migration {script_name}: failed (exit {s})"));
                state.insert(
                    script_name.clone(),
                    migrate::Record {
                        status: "failed".into(),
                        applied_at: chrono::Utc::now().to_rfc3339(),
                    },
                );
                migrate::save(&state_path, &state);
                // Continue — don't stop on failure
            }
            Err(e) => {
                log(
                    cfg,
                    &format!("migration {script_name}: error starting script: {e}"),
                );
                // Continue
            }
        }
    }

    // ── 4. Order services: non-server first (alphabetical), acc-server last ───
    let mut ordered: Vec<String> = restart_names.into_iter().collect();
    ordered.sort();
    if let Some(pos) = ordered.iter().position(|n| n == "acc-server") {
        let server = ordered.remove(pos);
        ordered.push(server);
    }

    // ── 5. Restart services ───────────────────────────────────────────────────
    let mut needs_self_restart = false;

    for svc_name in &ordered {
        if services::is_self(svc_name) {
            log(
                cfg,
                "acc-bus-listener restart requested — will re-exec after upgrade",
            );
            needs_self_restart = true;
            continue;
        }

        match services::find(svc_name) {
            None => {
                log(
                    cfg,
                    &format!("unknown service '{svc_name}' declared in migration — skipping"),
                );
            }
            Some(def) => {
                if opts.dry_run {
                    log(cfg, &format!("[dry-run] would restart {svc_name}"));
                    continue;
                }
                let cfg_ref = cfg;
                match services::restart(def, |msg| log(cfg_ref, msg)).await {
                    Ok(()) => log(cfg, &format!("restarted {svc_name} ok")),
                    Err(e) => log(cfg, &format!("WARNING: restart {svc_name} failed: {e}")),
                }
            }
        }
    }

    // ── 6. Post heartbeat ─────────────────────────────────────────────────────
    if !opts.dry_run {
        post_heartbeat(cfg).await;
    }

    // ── 7. Signal supervisor restart if needed ────────────────────────────────
    if needs_self_restart && !opts.dry_run {
        let pid_path = cfg.acc_dir.join("supervisor.pid");
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                log(cfg, &format!("sending SIGUSR1 to supervisor (pid {pid})"));
                let _ = std::process::Command::new("kill")
                    .args(["-USR1", &pid.to_string()])
                    .status();
                return; // supervisor will stop+restart all children including us
            }
        }
        // No supervisor running — fall back to re-exec self
        log(cfg, "no supervisor.pid found — re-execing self");
        let current_exe =
            std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("acc-agent"));
        let args: Vec<String> = std::env::args().collect();
        let _ = std::process::Command::new(&current_exe)
            .args(&args[1..])
            .spawn();
        std::process::exit(0);
    }

    log(cfg, "upgrade complete");
}

/// Parse `# Restarts: svc1 svc2 ...` from a migration script's source.
/// Only lines that start with `# Restarts:` are considered.
fn parse_restarts_header(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("# Restarts:")
        })
        .flat_map(|rest| rest.split_whitespace().map(String::from))
        .filter(|s| !s.is_empty())
        .collect()
}

fn workspace_rev(cfg: &Config) -> String {
    let workspace = cfg.acc_dir.join("workspace");
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(&workspace)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

async fn post_heartbeat(cfg: &Config) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let url = format!("{}/api/heartbeat/{}", cfg.acc_url, cfg.agent_name);
    let rev = workspace_rev(cfg);
    let body = serde_json::json!({
        "status": "ok",
        "note": "upgrade complete",
        "ccc_version": rev,
    });

    match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", cfg.acc_token))
        .json(&body)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            log(cfg, "heartbeat posted");
        }
        Ok(r) => log(cfg, &format!("WARNING: heartbeat returned {}", r.status())),
        Err(e) => log(cfg, &format!("WARNING: heartbeat failed: {e}")),
    }
}

fn log(cfg: &Config, msg: &str) {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let line = format!("[{ts}] [{}] [upgrade] {msg}", cfg.agent_name);
    eprintln!("{line}");
    tracing::info!(component = "upgrade", agent = %cfg.agent_name, "{msg}");
    let log_path = cfg.log_file("upgrade");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_restarts_header_single() {
        let src = "#!/usr/bin/env bash\n# Restarts: acc-hermes-worker\nset -euo pipefail\n";
        let svcs = parse_restarts_header(src);
        assert_eq!(svcs, vec!["acc-hermes-worker"]);
    }

    #[test]
    fn test_parse_restarts_header_multiple() {
        let src = "#!/usr/bin/env bash\n# Restarts: acc-hermes-worker acc-queue-worker\n";
        let mut svcs = parse_restarts_header(src);
        svcs.sort();
        assert_eq!(svcs, vec!["acc-hermes-worker", "acc-queue-worker"]);
    }

    #[test]
    fn test_parse_restarts_header_none() {
        let src = "#!/usr/bin/env bash\n# Description: does stuff\nset -euo pipefail\n";
        let svcs = parse_restarts_header(src);
        assert!(svcs.is_empty());
    }

    #[test]
    fn test_parse_restarts_header_multiple_lines() {
        let src = "# Restarts: acc-server\n# Restarts: acc-queue-worker\n";
        let mut svcs = parse_restarts_header(src);
        svcs.sort();
        assert_eq!(svcs, vec!["acc-queue-worker", "acc-server"]);
    }

    #[test]
    fn test_acc_server_goes_last() {
        // Simulate the ordering logic directly
        let names = ["acc-server", "acc-queue-worker", "acc-hermes-worker"];
        let mut ordered: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        ordered.sort();
        if let Some(pos) = ordered.iter().position(|n| n == "acc-server") {
            let server = ordered.remove(pos);
            ordered.push(server);
        }
        assert_eq!(ordered.last().unwrap(), "acc-server");
        assert_eq!(ordered[0], "acc-hermes-worker");
    }
}
