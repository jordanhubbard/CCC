//! Slack fleet-activity notifications.
//!
//! Posts compact claim/complete events to a single channel so the whole
//! fleet is observable in real-time.  Fire-and-forget: a failed POST is
//! logged but never propagates to the caller — task execution must not
//! depend on Slack availability.
//!
//! # Token resolution (same chain as the gateway)
//!
//! 1. `ACC_SLACK_NOTIFY_TOKEN` — explicit override for this notifier
//! 2. `SLACK_BOT_TOKEN`        — standard workspace default
//! 3. `SLACK_OMGJKH_TOKEN`     — legacy name used in the existing `.acc/.env`
//!
//! All of these are loaded from `~/.acc/.env` by `Config::load()` before
//! the main loops start, so they are already in the process environment
//! when these helpers are called.
//!
//! # Channel resolution
//!
//! `ACC_SLACK_NOTIFY_CHANNEL` env var (default: `fleet-activity`).
//!
//! # Message format
//!
//! Claim:    `:inbox_tray: *<agent>* claimed · <title> — <desc_snippet>\n<link>`
//! Complete: `:white_check_mark: *<agent>* completed · <title> — <result_snippet>\n<link>`
//!
//! Deliberately terse so 16 concurrent agents don't bury the channel.

use reqwest::Client as Http;
use serde_json::json;

const SLACK_POST: &str = "https://slack.com/api/chat.postMessage";
const DEFAULT_CHANNEL: &str = "fleet-activity";
const SNIPPET_LEN: usize = 120;

/// Build the hub UI deep-link for a task.
/// The dashboard is a single-page app; `#tasks` opens the tasks tab.
/// We append `?task=<id>` as a hint — the SPA can use it if it wants to
/// auto-open the modal, and the link is human-readable regardless.
fn task_link(acc_url: &str, task_id: &str) -> String {
    format!("{acc_url}/#tasks?task={task_id}")
}

fn bot_token() -> Option<String> {
    // 1. Explicit per-notifier override
    if let Ok(t) = std::env::var("ACC_SLACK_NOTIFY_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    // 2. Standard bot token
    if let Ok(t) = std::env::var("SLACK_BOT_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    // 3. Legacy omgjkh token (what the existing .acc/.env uses)
    if let Ok(t) = std::env::var("SLACK_OMGJKH_TOKEN") {
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

fn notify_channel() -> String {
    std::env::var("ACC_SLACK_NOTIFY_CHANNEL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CHANNEL.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        s.to_string()
    } else {
        // Break at a word boundary when possible
        let boundary = s
            .char_indices()
            .take_while(|(i, _)| *i < max.saturating_sub(1))
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max);
        format!("{}…", s[..boundary].trim_end())
    }
}

async fn post(token: &str, channel: &str, text: &str) -> Result<(), String> {
    let http = Http::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| format!("http build: {e}"))?;

    let body = json!({
        "channel": channel,
        "text":    text,
        "mrkdwn":  true,
        "unfurl_links": false,
        "unfurl_media": false,
    });

    let resp: serde_json::Value = http
        .post(SLACK_POST)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("response parse: {e}"))?;

    if !resp["ok"].as_bool().unwrap_or(false) {
        let err = resp["error"].as_str().unwrap_or("unknown_error");
        // channel_not_found is the most common misconfiguration; emit a
        // helpful hint so operators know what to set.
        if err == "channel_not_found" {
            return Err(format!(
                "channel_not_found — create #{channel} and invite the bot, \
                 or set ACC_SLACK_NOTIFY_CHANNEL to an existing channel"
            ));
        }
        return Err(format!("Slack API: {err}"));
    }
    Ok(())
}

/// Post `:inbox_tray: *<agent>* claimed · <title> — <desc_snippet>` to the
/// fleet-activity channel.  Fire-and-forget: errors are printed but not
/// returned.
///
/// * `acc_url`     — base URL of the hub (for deep-link)
/// * `agent_name`  — this agent's name
/// * `task_id`     — task ID
/// * `title`       — task title
/// * `description` — task description (truncated in message)
pub async fn notify_claimed(
    acc_url: &str,
    agent_name: &str,
    task_id: &str,
    title: &str,
    description: &str,
) {
    let Some(token) = bot_token() else {
        return; // Slack not configured — silent no-op
    };
    let channel = notify_channel();
    let link = task_link(acc_url, task_id);

    let desc_part = if description.trim().is_empty() {
        String::new()
    } else {
        format!(" — {}", truncate(description, SNIPPET_LEN))
    };

    let text = format!(
        ":inbox_tray: *{agent_name}* claimed · *{title}*{desc_part}\n{link}"
    );

    if let Err(e) = post(&token, &channel, &text).await {
        eprintln!("[slack_notify] claimed post failed: {e}");
    }
}

/// Post `:white_check_mark: *<agent>* completed · <title> — <result_snippet>`
/// to the fleet-activity channel.  Fire-and-forget.
///
/// * `result` — the task output / completion summary (truncated in message)
pub async fn notify_completed(
    acc_url: &str,
    agent_name: &str,
    task_id: &str,
    title: &str,
    result: &str,
) {
    let Some(token) = bot_token() else {
        return;
    };
    let channel = notify_channel();
    let link = task_link(acc_url, task_id);

    let result_part = if result.trim().is_empty() {
        String::new()
    } else {
        format!(" — {}", truncate(result, SNIPPET_LEN))
    };

    let text = format!(
        ":white_check_mark: *{agent_name}* completed · *{title}*{result_part}\n{link}"
    );

    if let Err(e) = post(&token, &channel, &text).await {
        eprintln!("[slack_notify] completed post failed: {e}");
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure-logic tests (no env mutation, safe to run concurrently) ──────────

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 120), "hello");
    }

    #[test]
    fn truncate_long_string_gets_ellipsis() {
        let s = "a".repeat(200);
        let result = truncate(&s, 120);
        assert!(result.ends_with('…'));
        // Byte length should be at most max + the UTF-8 size of '…' (3 bytes)
        assert!(result.len() <= 120 + 3);
    }

    #[test]
    fn truncate_trims_whitespace() {
        assert_eq!(truncate("  hi  ", 120), "hi");
    }

    #[test]
    fn task_link_format() {
        let link = task_link("https://hub.example.com", "task-abc123");
        assert_eq!(link, "https://hub.example.com/#tasks?task=task-abc123");
    }

    // ── Env-sensitive tests — all in ONE test fn to avoid data races ──────────
    //
    // Rust runs `#[test]` fns on a thread pool concurrently.  Because
    // `std::env` is process-global, any test that sets/removes a var can
    // race with another test that reads the same var.  Consolidating all
    // env-mutating assertions into a single test function that executes
    // them sequentially is the zero-dependency fix.

    #[test]
    fn env_var_resolution_sequential() {
        // ── 1. No token vars set → bot_token() returns None ──────────────────
        unsafe {
            std::env::remove_var("ACC_SLACK_NOTIFY_TOKEN");
            std::env::remove_var("SLACK_BOT_TOKEN");
            std::env::remove_var("SLACK_OMGJKH_TOKEN");
        }
        assert!(bot_token().is_none(), "expected None when no token vars set");

        // ── 2. Legacy SLACK_OMGJKH_TOKEN is picked up ────────────────────────
        unsafe {
            std::env::set_var("SLACK_OMGJKH_TOKEN", "xoxb-test-legacy");
        }
        assert_eq!(
            bot_token(),
            Some("xoxb-test-legacy".to_string()),
            "expected legacy token"
        );
        unsafe {
            std::env::remove_var("SLACK_OMGJKH_TOKEN");
        }

        // ── 3. SLACK_BOT_TOKEN takes precedence over SLACK_OMGJKH_TOKEN ──────
        unsafe {
            std::env::set_var("SLACK_BOT_TOKEN", "xoxb-standard");
            std::env::set_var("SLACK_OMGJKH_TOKEN", "xoxb-legacy");
        }
        assert_eq!(
            bot_token(),
            Some("xoxb-standard".to_string()),
            "SLACK_BOT_TOKEN should win over SLACK_OMGJKH_TOKEN"
        );
        unsafe {
            std::env::remove_var("SLACK_BOT_TOKEN");
            std::env::remove_var("SLACK_OMGJKH_TOKEN");
        }

        // ── 4. ACC_SLACK_NOTIFY_TOKEN takes highest precedence ───────────────
        unsafe {
            std::env::set_var("ACC_SLACK_NOTIFY_TOKEN", "xoxb-notify-override");
            std::env::set_var("SLACK_BOT_TOKEN", "xoxb-standard");
        }
        assert_eq!(
            bot_token(),
            Some("xoxb-notify-override".to_string()),
            "ACC_SLACK_NOTIFY_TOKEN should have highest precedence"
        );
        unsafe {
            std::env::remove_var("ACC_SLACK_NOTIFY_TOKEN");
            std::env::remove_var("SLACK_BOT_TOKEN");
        }

        // ── 5. Default channel when var is unset ─────────────────────────────
        unsafe {
            std::env::remove_var("ACC_SLACK_NOTIFY_CHANNEL");
        }
        assert_eq!(notify_channel(), "fleet-activity");

        // ── 6. Channel override ───────────────────────────────────────────────
        unsafe {
            std::env::set_var("ACC_SLACK_NOTIFY_CHANNEL", "ccc-ops");
        }
        assert_eq!(notify_channel(), "ccc-ops");
        unsafe {
            std::env::remove_var("ACC_SLACK_NOTIFY_CHANNEL");
        }

        // ── 7. Empty string vars are treated as unset ─────────────────────────
        unsafe {
            std::env::set_var("ACC_SLACK_NOTIFY_TOKEN", "");
            std::env::set_var("SLACK_BOT_TOKEN", "");
            std::env::set_var("SLACK_OMGJKH_TOKEN", "");
        }
        assert!(bot_token().is_none(), "empty token vars should be treated as unset");
        unsafe {
            std::env::remove_var("ACC_SLACK_NOTIFY_TOKEN");
            std::env::remove_var("SLACK_BOT_TOKEN");
            std::env::remove_var("SLACK_OMGJKH_TOKEN");
        }

        // ── 8. Empty channel string falls back to default ─────────────────────
        unsafe {
            std::env::set_var("ACC_SLACK_NOTIFY_CHANNEL", "");
        }
        assert_eq!(notify_channel(), "fleet-activity", "empty channel should fall back to default");
        unsafe {
            std::env::remove_var("ACC_SLACK_NOTIFY_CHANNEL");
        }
    }
}
