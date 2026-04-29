use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::session_discovery;
use crate::tmux;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(800);
const BUSY_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const CAPTURE_LINES: usize = 400;

pub struct SessionAdapterConfig {
    pub executor: &'static str,
    pub launch_binary: &'static str,
    pub launch_args: &'static [&'static str],
    pub default_session_name: &'static str,
    pub env_session_name: &'static str,
}

pub struct SessionTaskResult {
    pub output: String,
}

pub async fn run_task(
    cfg: &Config,
    adapter: &SessionAdapterConfig,
    workspace: &Path,
    task_id: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<SessionTaskResult, String> {
    let session_name = ensure_session(cfg, adapter, workspace).await?;
    let pane_id = pane_for_session(&session_name).await?;

    wait_until_idle(&pane_id, BUSY_WAIT_TIMEOUT)
        .await
        .map_err(|e| format!("session_not_idle:{e}"))?;

    let task_prompt = format!(
        "ACC_QUEUE_ITEM {task_id}\n{prompt}\n\nWhen you finish, stop and wait for the next instruction."
    );
    let buffer_name = format!("acc-{}-{}", task_id, std::process::id());
    tmux::set_buffer(&buffer_name, &task_prompt).await?;
    tmux::paste_buffer(&pane_id, &buffer_name).await?;
    tmux::send_keys(&pane_id, "", true).await?;

    let pane_text = wait_for_completion(&pane_id, timeout).await?;
    Ok(SessionTaskResult {
        output: extract_task_output(&pane_text, task_id),
    })
}

async fn ensure_session(
    _cfg: &Config,
    adapter: &SessionAdapterConfig,
    workspace: &Path,
) -> Result<String, String> {
    if let Ok(name) = std::env::var(adapter.env_session_name) {
        if !name.trim().is_empty() {
            return Ok(name);
        }
    }

    if let Some(existing) = detect_existing_session(adapter.executor, workspace).await {
        return Ok(existing);
    }

    let launch = render_launch(adapter);
    tmux::new_session(adapter.default_session_name, Some(workspace), &launch).await?;
    let pane_id = pane_for_session(adapter.default_session_name).await?;
    wait_until_idle(&pane_id, STARTUP_TIMEOUT).await?;
    Ok(adapter.default_session_name.to_string())
}

async fn detect_existing_session(executor: &str, workspace: &Path) -> Option<String> {
    let panes = tmux::list_panes().await.ok()?;
    let workspace = workspace.to_string_lossy();
    panes
        .into_iter()
        .filter(|pane| !pane.dead)
        .filter(|pane| session_discovery::infer_executor(pane) == Some(executor))
        .min_by_key(|pane| {
            let exact_path = if pane.current_path == workspace { 0 } else { 1 };
            let active = if pane.active || pane.window_active {
                0
            } else {
                1
            };
            (exact_path, active, pane.session_name.clone())
        })
        .map(|pane| pane.session_name)
}

async fn pane_for_session(session_name: &str) -> Result<String, String> {
    let panes = tmux::list_panes().await?;
    panes
        .into_iter()
        .find(|pane| pane.session_name == session_name && !pane.dead)
        .map(|pane| pane.pane_id)
        .ok_or_else(|| format!("session_not_found:{session_name}"))
}

async fn wait_until_idle(pane_id: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let capture = tmux::capture_pane(pane_id, CAPTURE_LINES).await?;
        if is_idle_prompt_visible(&capture) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("timeout_waiting_for_idle".into());
        }
        tokio::time::sleep(IDLE_POLL_INTERVAL).await;
    }
}

async fn wait_for_completion(pane_id: &str, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let capture = tmux::capture_pane(pane_id, CAPTURE_LINES).await?;
        if is_idle_prompt_visible(&capture) {
            return Ok(capture);
        }
        if Instant::now() >= deadline {
            return Err("timeout_waiting_for_session_completion".into());
        }
        tokio::time::sleep(IDLE_POLL_INTERVAL).await;
    }
}

fn render_launch(adapter: &SessionAdapterConfig) -> String {
    if adapter.launch_args.is_empty() {
        adapter.launch_binary.to_string()
    } else {
        format!(
            "{} {}",
            adapter.launch_binary,
            adapter.launch_args.join(" ")
        )
    }
}

fn is_idle_prompt_visible(capture: &str) -> bool {
    let cleaned = strip_ansi(capture);
    let last_lines: Vec<&str> = cleaned.lines().rev().take(6).collect();
    let has_prompt = last_lines.iter().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("❯ ")
            || trimmed.starts_with("> ")
            || trimmed.contains("? for shortcuts")
    });
    let has_spinner = last_lines
        .iter()
        .any(|line| line.chars().any(is_spinner_char));
    has_prompt && !has_spinner
}

fn is_spinner_char(ch: char) -> bool {
    matches!(
        ch,
        '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏'
    )
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn extract_task_output(capture: &str, task_id: &str) -> String {
    let cleaned = strip_ansi(capture);
    let marker = format!("ACC_QUEUE_ITEM {task_id}");
    if let Some(idx) = cleaned.rfind(&marker) {
        cleaned[idx..].trim().to_string()
    } else {
        cleaned.trim().to_string()
    }
}

pub fn claude_adapter() -> SessionAdapterConfig {
    SessionAdapterConfig {
        executor: "claude_cli",
        launch_binary: "claude",
        launch_args: &["--dangerously-skip-permissions"],
        default_session_name: "claude-main",
        env_session_name: "ACC_CLAUDE_SESSION",
    }
}

pub fn codex_adapter() -> SessionAdapterConfig {
    SessionAdapterConfig {
        executor: "codex_cli",
        launch_binary: "codex",
        launch_args: &["--sandbox", "danger-full-access", "--full-auto"],
        default_session_name: "codex-main",
        env_session_name: "ACC_CODEX_SESSION",
    }
}

pub fn cursor_adapter() -> SessionAdapterConfig {
    SessionAdapterConfig {
        executor: "cursor_cli",
        launch_binary: "cursor",
        launch_args: &["--headless"],
        default_session_name: "cursor-main",
        env_session_name: "ACC_CURSOR_SESSION",
    }
}

pub fn adapter_for_executor(executor: &str) -> Option<SessionAdapterConfig> {
    match executor {
        "claude_cli" => Some(claude_adapter()),
        "codex_cli" => Some(codex_adapter()),
        "cursor_cli" => Some(cursor_adapter()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_prompt_detection_matches_readme_contract() {
        assert!(is_idle_prompt_visible("done\n❯ "));
        assert!(is_idle_prompt_visible("status\n? for shortcuts"));
        assert!(!is_idle_prompt_visible("working\n⠋ applying patch\n❯ "));
        assert!(!is_idle_prompt_visible("just output"));
    }

    #[test]
    fn extract_task_output_prefers_latest_marker() {
        let capture = "old\nACC_QUEUE_ITEM a\none\nACC_QUEUE_ITEM b\ntwo";
        assert_eq!(extract_task_output(capture, "b"), "ACC_QUEUE_ITEM b\ntwo");
    }

    #[test]
    fn adapter_for_executor_maps_known_executors() {
        assert_eq!(
            adapter_for_executor("claude_cli")
                .unwrap()
                .default_session_name,
            "claude-main"
        );
        assert_eq!(
            adapter_for_executor("codex_cli")
                .unwrap()
                .default_session_name,
            "codex-main"
        );
        assert_eq!(
            adapter_for_executor("cursor_cli")
                .unwrap()
                .default_session_name,
            "cursor-main"
        );
        assert!(adapter_for_executor("gpu").is_none());
    }
}
