use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::tmux;

const PROBE_TIMEOUT: Duration = Duration::from_secs(45);
const PROBE_QUIET_SECS: u64 = 8;
const PROBE_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const PROBE_QUESTION: &str = "Reply with only one lowercase word: what is the capital of France?";
const PROBE_EXPECTED_ANSWER: &str = "paris";

// Canonical contents for an ACC-managed Claude CLI agent's settings file.
// Written to $HOME/.claude/settings.json before the Claude vetting probe so
// the agent never hits a permission prompt during automated execution.
const CLAUDE_SETTINGS_JSON: &str = r#"{
  "permissions": {
    "allow": [
      "Bash",
      "Edit",
      "Write",
      "Read",
      "WebFetch",
      "WebSearch",
      "mcp__*"
    ],
    "defaultMode": "dontAsk"
  },
  "enabledPlugins": {
    "swift-lsp@claude-plugins-official": true,
    "clangd-lsp@claude-plugins-official": true,
    "rust-analyzer-lsp@claude-plugins-official": true
  },
  "theme": "light",
  "model": "opus[1m]"
}
"#;

#[derive(Debug, Clone)]
struct ProbeCacheEntry {
    checked_at: Instant,
    healthy: bool,
    detail: String,
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub healthy: bool,
    pub detail: String,
}

fn probe_cache() -> &'static Mutex<HashMap<String, ProbeCacheEntry>> {
    static CELL: OnceLock<Mutex<HashMap<String, ProbeCacheEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn choose_ready_executor(
    cfg: &Config,
    preferred_executor: Option<&str>,
    workspace: &Path,
) -> Option<String> {
    let mut candidates = Vec::new();
    if let Some(preferred) = preferred_executor.filter(|s| is_supported_executor(s)) {
        candidates.push(preferred.to_string());
    }
    for fallback in ["claude_cli", "codex_cli", "cursor_cli"] {
        if !candidates.iter().any(|c| c == fallback) {
            candidates.push(fallback.to_string());
        }
    }

    for executor in candidates {
        let result = ensure_executor_ready(cfg, &executor, workspace).await;
        if result.healthy {
            return Some(executor);
        }
    }
    None
}

pub async fn ensure_executor_ready(cfg: &Config, executor: &str, workspace: &Path) -> ProbeResult {
    if let Some(cached) = cached_probe(executor) {
        return ProbeResult {
            healthy: cached.healthy,
            detail: cached.detail,
        };
    }

    let result = run_probe(cfg, executor, workspace).await;
    if let Ok(mut cache) = probe_cache().lock() {
        cache.insert(
            executor.to_string(),
            ProbeCacheEntry {
                checked_at: Instant::now(),
                healthy: result.healthy,
                detail: result.detail.clone(),
            },
        );
    }
    result
}

fn cached_probe(executor: &str) -> Option<ProbeCacheEntry> {
    let cache = probe_cache().lock().ok()?;
    let entry = cache.get(executor)?;
    if entry.checked_at.elapsed() <= PROBE_CACHE_TTL {
        Some(entry.clone())
    } else {
        None
    }
}

async fn run_probe(_cfg: &Config, executor: &str, workspace: &Path) -> ProbeResult {
    if executor == "claude_cli" {
        if let Err(e) = ensure_claude_settings() {
            return ProbeResult {
                healthy: false,
                detail: format!("settings_write_failed:{e}"),
            };
        }
    }

    let Some(command) = launch_command(executor) else {
        return ProbeResult {
            healthy: false,
            detail: "unsupported_executor".into(),
        };
    };
    if which_bin(command.binary).is_none() {
        return ProbeResult {
            healthy: false,
            detail: "binary_missing".into(),
        };
    }

    let session_name = format!(
        "acc-probe-{}-{}",
        executor.replace('_', "-"),
        std::process::id()
    );
    let launch = command.render();
    let started = tmux::new_session(&session_name, Some(workspace), &launch).await;
    if let Err(e) = started {
        return ProbeResult {
            healthy: false,
            detail: format!("launch_failed:{e}"),
        };
    }

    let pane_id = match wait_for_probe_pane(&session_name).await {
        Some(id) => id,
        None => {
            let _ = tmux::kill_session(&session_name).await;
            return ProbeResult {
                healthy: false,
                detail: "pane_not_found".into(),
            };
        }
    };

    tokio::time::sleep(Duration::from_secs(2)).await;
    let send_res = tmux::send_keys(&pane_id, PROBE_QUESTION, true).await;
    if let Err(e) = send_res {
        let _ = tmux::kill_session(&session_name).await;
        return ProbeResult {
            healthy: false,
            detail: format!("send_failed:{e}"),
        };
    }

    let quiet = tmux::wait_for_quiet(&pane_id, PROBE_QUIET_SECS, PROBE_TIMEOUT.as_secs()).await;
    let capture = tmux::capture_pane(&pane_id, 80).await.unwrap_or_default();
    let _ = tmux::kill_session(&session_name).await;

    let quiet = match quiet {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                healthy: false,
                detail: format!("quiet_wait_failed:{e}"),
            };
        }
    };
    if !quiet {
        return ProbeResult {
            healthy: false,
            detail: "probe_timeout".into(),
        };
    }

    if probe_passed(&capture) {
        ProbeResult {
            healthy: true,
            detail: "probe_passed".into(),
        }
    } else {
        ProbeResult {
            healthy: false,
            detail: "probe_failed_non_llm_answer".into(),
        }
    }
}

async fn wait_for_probe_pane(session_name: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(panes) = tmux::list_panes().await {
            if let Some(pane) = panes.into_iter().find(|p| p.session_name == session_name) {
                return Some(pane.pane_id);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn probe_passed(capture: &str) -> bool {
    let normalized = capture.to_ascii_lowercase();
    normalized.lines().any(|line| {
        let line = line.trim();
        line == PROBE_EXPECTED_ANSWER
            || line.ends_with(&format!(" {}", PROBE_EXPECTED_ANSWER))
            || line.contains(&format!(">{}", PROBE_EXPECTED_ANSWER))
    })
}

fn is_supported_executor(executor: &str) -> bool {
    matches!(executor, "claude_cli" | "codex_cli" | "cursor_cli")
}

/// Writes the canonical ACC-managed Claude settings to
/// `$HOME/.claude/settings.json`, creating the directory if needed.
/// Idempotent: re-reads the file first and skips the write when the
/// content already matches.
fn ensure_claude_settings() -> std::io::Result<()> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
    let dir = PathBuf::from(home).join(".claude");
    let file = dir.join("settings.json");

    if let Ok(existing) = std::fs::read_to_string(&file) {
        if existing == CLAUDE_SETTINGS_JSON {
            return Ok(());
        }
    }

    std::fs::create_dir_all(&dir)?;
    std::fs::write(&file, CLAUDE_SETTINGS_JSON)?;
    Ok(())
}

fn which_bin(name: &str) -> Option<PathBuf> {
    std::env::var("PATH").ok().and_then(|path_var| {
        path_var.split(':').find_map(|dir| {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}

struct LaunchCommand {
    binary: &'static str,
    args: &'static [&'static str],
}

impl LaunchCommand {
    fn render(&self) -> String {
        if self.args.is_empty() {
            self.binary.to_string()
        } else {
            format!("{} {}", self.binary, self.args.join(" "))
        }
    }
}

fn launch_command(executor: &str) -> Option<LaunchCommand> {
    match executor {
        "claude_cli" => Some(LaunchCommand {
            binary: "claude",
            args: &["--dangerously-skip-permissions"],
        }),
        "codex_cli" => Some(LaunchCommand {
            binary: "codex",
            args: &["--sandbox", "danger-full-access", "--full-auto"],
        }),
        "cursor_cli" => Some(LaunchCommand {
            binary: "cursor",
            args: &["--headless"],
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_passed_detects_expected_llm_answer() {
        assert!(probe_passed("capital?\nparis\n"));
        assert!(probe_passed("assistant> paris"));
        assert!(!probe_passed("bash: Reply: command not found"));
    }

    #[test]
    fn launch_command_maps_supported_executors() {
        assert_eq!(launch_command("claude_cli").unwrap().binary, "claude");
        assert!(launch_command("unknown").is_none());
    }
}
