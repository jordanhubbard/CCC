use std::collections::BTreeMap;
use std::path::PathBuf;

use acc_model::{AgentExecutor, AgentSession};
use chrono::{TimeZone, Utc};

use crate::config::Config;
use crate::tmux::{self, PaneInfo};

#[derive(Debug, Clone, Default)]
pub struct DiscoverySnapshot {
    pub executors: Vec<AgentExecutor>,
    pub sessions: Vec<AgentSession>,
}

pub async fn discover(cfg: &Config) -> DiscoverySnapshot {
    let panes = tmux::list_panes().await.unwrap_or_default();
    let mut sessions = Vec::new();
    for pane in panes {
        if let Some(executor) = infer_executor(&pane) {
            sessions.push(classify_session(cfg, &pane, executor).await);
        }
    }

    let executors = supported_executors()
        .into_iter()
        .map(|executor| build_executor_status(cfg, executor, &sessions))
        .collect();

    DiscoverySnapshot {
        executors,
        sessions,
    }
}

pub fn supported_executors() -> Vec<&'static str> {
    vec![
        "claude_cli",
        "codex_cli",
        "cursor_cli",
        "opencode",
        "inference_key",
    ]
}

pub fn infer_executor(pane: &PaneInfo) -> Option<&'static str> {
    let haystack = format!(
        "{} {} {} {}",
        pane.session_name, pane.pane_title, pane.current_command, pane.start_command
    )
    .to_ascii_lowercase();

    if haystack.contains("claude") {
        Some("claude_cli")
    } else if haystack.contains("codex") {
        Some("codex_cli")
    } else if haystack.contains("cursor") {
        Some("cursor_cli")
    } else if haystack.contains("opencode") {
        Some("opencode")
    } else {
        None
    }
}

async fn classify_session(cfg: &Config, pane: &PaneInfo, executor: &str) -> AgentSession {
    let capture = tmux::capture_pane(&pane.pane_id, 20)
        .await
        .unwrap_or_default();
    let auth_state = classify_auth_state(cfg, executor, &capture);
    let activity_epoch = pane
        .activity_epoch
        .unwrap_or_else(|| Utc::now().timestamp());
    let last_activity = Utc.timestamp_opt(activity_epoch, 0).single();
    let age_secs = Utc::now().timestamp() - activity_epoch;
    let looks_idle = looks_idle_prompt(&capture);
    let state = if pane.dead {
        "dead"
    } else if auth_state == "unauthenticated" {
        "unauthenticated"
    } else if age_secs > cfg.session_stuck_threshold_secs() && !looks_idle {
        "stuck"
    } else if age_secs <= cfg.session_busy_window_secs() {
        "busy"
    } else {
        "idle"
    };

    let mut extra = BTreeMap::new();
    extra.insert("pane_id".into(), serde_json::json!(pane.pane_id));
    extra.insert("current_path".into(), serde_json::json!(pane.current_path));
    extra.insert("activity_age_secs".into(), serde_json::json!(age_secs));

    AgentSession {
        name: pane.session_name.clone(),
        executor: Some(executor.to_string()),
        project_id: derive_project_binding(&pane.session_name, executor),
        state: Some(state.to_string()),
        auth_state: Some(auth_state.to_string()),
        last_activity,
        busy: Some(matches!(state, "busy" | "stuck")),
        stuck: Some(state == "stuck"),
        estimated_ram_mb: Some(default_session_ram_mb(executor)),
        extra,
    }
}

fn build_executor_status(cfg: &Config, executor: &str, sessions: &[AgentSession]) -> AgentExecutor {
    let executor_sessions: Vec<&AgentSession> = sessions
        .iter()
        .filter(|s| s.executor.as_deref() == Some(executor))
        .collect();
    let installed = match executor {
        "claude_cli" => which_bin("claude").is_some(),
        "codex_cli" => which_bin("codex").is_some(),
        "cursor_cli" => which_bin("cursor").is_some(),
        "opencode" => which_bin("opencode").is_some(),
        "inference_key" => env_inference_ready(),
        _ => false,
    };
    let auth_state = if executor_sessions
        .iter()
        .any(|s| s.auth_state.as_deref() == Some("ready"))
    {
        "ready"
    } else if executor_sessions
        .iter()
        .any(|s| s.auth_state.as_deref() == Some("unauthenticated"))
    {
        "unauthenticated"
    } else if installed && env_auth_ready(cfg, executor) {
        "ready"
    } else if installed {
        "unknown"
    } else {
        "missing"
    };
    let ready = installed && auth_state != "unauthenticated";

    let mut extra = BTreeMap::new();
    extra.insert("type".into(), serde_json::json!(executor));
    extra.insert(
        "session_count".into(),
        serde_json::json!(executor_sessions.len()),
    );

    AgentExecutor {
        executor: executor.to_string(),
        ready: Some(ready),
        auth_state: Some(auth_state.to_string()),
        installed: Some(installed),
        extra,
    }
}

fn classify_auth_state(cfg: &Config, executor: &str, capture: &str) -> &'static str {
    let lower = capture.to_ascii_lowercase();
    if lower.contains("sign in")
        || lower.contains("authenticate")
        || lower.contains("login required")
        || lower.contains("session token")
    {
        return "unauthenticated";
    }
    if env_auth_ready(cfg, executor) {
        "ready"
    } else {
        "unknown"
    }
}

fn env_auth_ready(cfg: &Config, executor: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_else(|_| cfg.acc_dir.to_string_lossy().to_string());
    match executor {
        "claude_cli" => {
            std::env::var("ANTHROPIC_API_KEY")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
                || PathBuf::from(&home).join(".claude/credentials").exists()
        }
        "codex_cli" => std::env::var("OPENAI_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false),
        "cursor_cli" => {
            std::env::var("CURSOR_SESSION_TOKEN")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
                || PathBuf::from(&home).join(".cursor/session").exists()
        }
        "opencode" => std::env::var("OPENAI_API_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false),
        "inference_key" => env_inference_ready(),
        _ => false,
    }
}

fn env_inference_ready() -> bool {
    ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "NVIDIA_API_KEY"]
        .iter()
        .any(|key| std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false))
}

fn derive_project_binding(session_name: &str, executor: &str) -> Option<String> {
    let mut trimmed = session_name.to_string();
    for prefix in [
        executor,
        executor.trim_end_matches("_cli"),
        "claude",
        "codex",
        "cursor",
        "opencode",
    ] {
        let colon = format!("{prefix}:");
        let dash = format!("{prefix}-");
        if let Some(rest) = trimmed.strip_prefix(&colon) {
            trimmed = rest.to_string();
            break;
        }
        if let Some(rest) = trimmed.strip_prefix(&dash) {
            trimmed = rest.to_string();
            break;
        }
    }
    let normalized = trimmed.trim().trim_matches('/');
    if normalized.is_empty() || normalized == session_name {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn looks_idle_prompt(capture: &str) -> bool {
    let last = capture
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    last.ends_with('$')
        || last.ends_with('%')
        || last.ends_with('>')
        || last.contains("Ready")
        || last.contains("ready")
        || last.ends_with(':')
}

fn default_session_ram_mb(executor: &str) -> u64 {
    match executor {
        "cursor_cli" => 2200,
        "claude_cli" | "codex_cli" => 1600,
        "opencode" => 1200,
        _ => 512,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str, current_command: &str, start_command: &str) -> PaneInfo {
        PaneInfo {
            session_name: name.to_string(),
            pane_id: "%1".into(),
            pane_pid: Some(1),
            current_command: current_command.to_string(),
            current_path: "/tmp".into(),
            pane_title: current_command.to_string(),
            active: true,
            window_active: true,
            dead: false,
            start_command: start_command.to_string(),
            activity_epoch: Some(Utc::now().timestamp()),
        }
    }

    #[test]
    fn infer_executor_from_tmux_metadata() {
        assert_eq!(
            infer_executor(&pane("claude:proj", "bash", "claude")),
            Some("claude_cli")
        );
        assert_eq!(
            infer_executor(&pane(
                "codex-main",
                "bash",
                "codex --sandbox danger-full-access --full-auto"
            )),
            Some("codex_cli")
        );
        assert_eq!(infer_executor(&pane("misc", "zsh", "sleep 5")), None);
    }

    #[test]
    fn derive_project_binding_from_session_name() {
        assert_eq!(
            derive_project_binding("claude:proj-a", "claude_cli").as_deref(),
            Some("proj-a")
        );
        assert_eq!(
            derive_project_binding("codex-main", "codex_cli").as_deref(),
            Some("main")
        );
        assert_eq!(derive_project_binding("cursor", "cursor_cli"), None);
    }

    #[test]
    fn looks_idle_prompt_matches_common_shell_prompts() {
        assert!(looks_idle_prompt("done\nacc%"));
        assert!(looks_idle_prompt("Ready"));
        assert!(!looks_idle_prompt("Applying patch to file"));
    }
}
