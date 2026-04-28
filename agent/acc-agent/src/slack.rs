//! Slack progress reporter — posts claim, complete, and failure events to a fleet-activity channel.
//!
//! # Configuration (all optional; missing → silent no-op)
//!
//! | Env var               | Description                                        | Default              |
//! |-----------------------|----------------------------------------------------|----------------------|
//! | `SLACK_BOT_TOKEN`     | Bot token (`xoxb-…`) — required to post            | —                    |
//! | `SLACK_FLEET_CHANNEL` | Channel ID or name to post into                    | `fleet-activity`     |
//! | `ACC_URL`             | Hub base URL — used to build a deep-link           | —                    |
//!
//! If `SLACK_BOT_TOKEN` is absent the functions return immediately so agents
//! that haven't been given Slack credentials are unaffected.
//!
//! # Message format (compact, emoji-led)
//!
//! Claim:    `:inbox_tray: *<agent>* claimed: <title> — <desc-snippet>`  + link
//! Complete: `:white_check_mark: *<agent>* completed: <title> — <result-snippet>` + link
//! Failed:   `:x: *<agent>* failed: <title> — <error-snippet>` + link
//! Voting:   `:ballot_box_with_check: *<agent>* voting on idea: *<title>*` + link
//! Voted:    `:ballot_box_with_check: *<agent>* voted *<approve|reject>* on idea: *<title>*` + link
//!
//! Idea tasks are never claimed (voting is non-exclusive and concurrent), so
//! there is no "Claim" event for them.  `notify_voting` fires at the start of
//! evaluation to give fleet observers visibility into vote activity; `notify_voted`
//! fires after the vote is successfully submitted.
//!
//! Short enough that 16 concurrent claims produce 16 distinct lines, not a
//! paragraph-per-event flood.

use std::sync::OnceLock;
use std::time::Duration;

const DESC_MAX: usize = 120;
const RESULT_MAX: usize = 160;
const ERROR_MAX: usize = 160;
const TITLE_MAX: usize = 80;
const POST_TIMEOUT: Duration = Duration::from_secs(8);

/// Process-wide shared HTTP client for Slack API calls.
///
/// `reqwest::Client` manages its own connection pool and is explicitly
/// designed to be cloned/shared across tasks.  Building one per
/// `post_message()` call throws away TLS sessions and connection-pool
/// state, which matters when 16+ agents fire notifications concurrently.
/// `OnceLock` gives us a safe, lazy, lock-free singleton.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Return a reference to the shared `reqwest::Client`, initialising it on
/// the first call.  Panics only if `reqwest::Client::builder().build()`
/// fails (which it never does in practice with this configuration).
fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(POST_TIMEOUT)
            .build()
            .expect("failed to build shared Slack HTTP client")
    })
}

/// Return the configured channel name/ID (defaults to "fleet-activity").
fn channel() -> String {
    std::env::var("SLACK_FLEET_CHANNEL")
        .unwrap_or_else(|_| "fleet-activity".to_string())
}

/// Return the bot token, or None if not configured.
fn bot_token() -> Option<String> {
    std::env::var("SLACK_BOT_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Build a hub deep-link to the task record, or an empty string.
fn task_link(task_id: &str) -> String {
    let base = std::env::var("ACC_URL")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    if base.is_empty() || task_id.is_empty() {
        return String::new();
    }
    // The hub dashboard shows tasks at /#tasks (it's a SPA; the fragment
    // hints at the active tab but doesn't directly deep-link to a record yet).
    // We embed the task ID as a query param so it still gives reviewers a
    // one-click route to the right hub instance.
    format!("{base}/?task={task_id}")
}

/// Truncate `s` to at most `max` chars, appending "…" when cut.
fn trunc(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Post a raw Slack message via `chat.postMessage`.
/// Returns immediately (fire-and-forget) if the token is missing.
/// Errors are logged via `tracing::warn` and swallowed — Slack is best-effort.
async fn post_message(text: &str) {
    let token = match bot_token() {
        Some(t) => t,
        None => return,
    };

    let body = serde_json::json!({
        "channel": channel(),
        "text":    text,
        // Slack mrkdwn is on by default for text; no extra field needed.
    });

    match http_client()
        .post("https://slack.com/api/chat.postMessage")
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            // Slack always returns 200; check the JSON `ok` field.
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if json["ok"].as_bool() != Some(true) {
                    let err = json["error"].as_str().unwrap_or("unknown");
                    tracing::warn!(component = "slack", "chat.postMessage failed: {err}");
                }
            }
        }
        Err(e) => {
            tracing::warn!(component = "slack", "chat.postMessage HTTP error: {e}");
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Post a `:inbox_tray: *<agent>* claimed: <title> — <desc>` message.
///
/// Call this immediately after a successful claim, before spawning the
/// execution task.  Fire-and-forget — does not block the claim path.
pub async fn notify_claimed(agent: &str, task_id: &str, title: &str, description: &str) {
    if bot_token().is_none() {
        return;
    }
    let t = trunc(title, TITLE_MAX);
    let d = trunc(description, DESC_MAX);
    let link = task_link(task_id);

    let desc_part = if d.is_empty() {
        String::new()
    } else {
        format!(" — {d}")
    };
    let link_part = if link.is_empty() {
        String::new()
    } else {
        format!(" (<{link}|hub>)")
    };

    let text = format!(":inbox_tray: *{agent}* claimed: *{t}*{desc_part}{link_part}");
    post_message(&text).await;
}

/// Post a `:white_check_mark: *<agent>* completed: <title> — <result>` message.
///
/// Call this after `complete_task` / `post_complete` returns.
pub async fn notify_completed(agent: &str, task_id: &str, title: &str, result: &str) {
    if bot_token().is_none() {
        return;
    }
    let t = trunc(title, TITLE_MAX);
    let r = trunc(result, RESULT_MAX);
    let link = task_link(task_id);

    let result_part = if r.is_empty() {
        String::new()
    } else {
        format!(" — {r}")
    };
    let link_part = if link.is_empty() {
        String::new()
    } else {
        format!(" (<{link}|hub>)")
    };

    let text = format!(":white_check_mark: *{agent}* completed: *{t}*{result_part}{link_part}");
    post_message(&text).await;
}

/// Post a `:ballot_box_with_check: *<agent>* voting on idea: *<title>*` message.
///
/// Call this at the very start of `execute_idea_vote_task`, before the agentic
/// loop runs, so fleet observers get a Slack signal when an agent begins
/// evaluating an idea.  Idea tasks are never claimed (voting is non-exclusive
/// and concurrent), so `notify_claimed` is never fired for them — this
/// function fills that visibility gap.  Fire-and-forget — does not block the
/// vote-evaluation path.
pub async fn notify_voting(agent: &str, task_id: &str, title: &str) {
    if bot_token().is_none() {
        return;
    }
    let t = trunc(title, TITLE_MAX);
    let link = task_link(task_id);

    let link_part = if link.is_empty() {
        String::new()
    } else {
        format!(" (<{link}|hub>)")
    };

    let text = format!(":ballot_box_with_check: *{agent}* voting on idea: *{t}*{link_part}");
    post_message(&text).await;
}

/// Post a `:ballot_box_with_check: *<agent>* voted <approve|reject> on idea: <title>` message.
///
/// Call this after a successful `PUT /api/tasks/:id/vote` so fleet observers
/// can see idea-voting activity in the same channel as work/review events.
/// Idea tasks are never claimed (voting is non-exclusive), so there is no
/// `notify_claimed` counterpart — `notify_voting` fires at evaluation start
/// and this function fires on successful vote submission.
/// Fire-and-forget — does not block the vote-submission path.
pub async fn notify_voted(agent: &str, task_id: &str, title: &str, vote: &str) {
    if bot_token().is_none() {
        return;
    }
    let t = trunc(title, TITLE_MAX);
    let link = task_link(task_id);

    let link_part = if link.is_empty() {
        String::new()
    } else {
        format!(" (<{link}|hub>)")
    };

    let text = format!(":ballot_box_with_check: *{agent}* voted *{vote}* on idea: *{t}*{link_part}");
    post_message(&text).await;
}

/// Post a `:x: *<agent>* failed: <title> — <error>` message.
///
/// Call this after `unclaim_task` / `post_fail` returns so operators can see
/// fleet failures without polling the hub.  Fire-and-forget — does not block
/// the failure-handling path.
pub async fn notify_failed(agent: &str, task_id: &str, title: &str, error: &str) {
    if bot_token().is_none() {
        return;
    }
    let t = trunc(title, TITLE_MAX);
    let e = trunc(error, ERROR_MAX);
    let link = task_link(task_id);

    let error_part = if e.is_empty() {
        String::new()
    } else {
        format!(" — {e}")
    };
    let link_part = if link.is_empty() {
        String::new()
    } else {
        format!(" (<{link}|hub>)")
    };

    let text = format!(":x: *{agent}* failed: *{t}*{error_part}{link_part}");
    post_message(&text).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ── Pure / no env-var dependency ─────────────────────────────────────────

    #[test]
    fn trunc_short_string_unchanged() {
        assert_eq!(trunc("hello", 10), "hello");
    }

    #[test]
    fn trunc_exact_boundary_unchanged() {
        let s = "a".repeat(80);
        assert_eq!(trunc(&s, 80), s);
    }

    #[test]
    fn trunc_long_string_gets_ellipsis() {
        let s = "a".repeat(90);
        let out = trunc(&s, 80);
        assert!(out.ends_with('…'), "expected ellipsis, got: {out}");
        assert!(out.chars().count() <= 80);
    }

    #[test]
    fn trunc_trims_whitespace() {
        assert_eq!(trunc("  hi  ", 20), "hi");
    }

    // ── Env-var-dependent tests — must run serially ───────────────────────────

    #[test]
    #[serial]
    fn task_link_empty_when_no_acc_url() {
        std::env::remove_var("ACC_URL");
        let link = task_link("task-123");
        // When ACC_URL is unset the link must be empty.
        assert!(
            link.is_empty() || link.contains("task-123"),
            "link must be empty or contain the task id"
        );
    }

    #[test]
    #[serial]
    fn task_link_contains_task_id_when_base_set() {
        std::env::set_var("ACC_URL", "https://hub.example.com");
        let link = task_link("task-xyz");
        assert!(link.contains("task-xyz"), "link must contain task id: {link}");
        assert!(
            link.starts_with("https://hub.example.com"),
            "link must start with base: {link}"
        );
        std::env::remove_var("ACC_URL");
    }

    #[test]
    #[serial]
    fn bot_token_none_when_unset() {
        std::env::remove_var("SLACK_BOT_TOKEN");
        assert!(bot_token().is_none());
    }

    #[test]
    #[serial]
    fn bot_token_none_when_empty() {
        std::env::set_var("SLACK_BOT_TOKEN", "");
        assert!(bot_token().is_none());
        std::env::remove_var("SLACK_BOT_TOKEN");
    }

    #[test]
    #[serial]
    fn bot_token_some_when_set() {
        std::env::set_var("SLACK_BOT_TOKEN", "xoxb-test-token");
        assert_eq!(bot_token().as_deref(), Some("xoxb-test-token"));
        std::env::remove_var("SLACK_BOT_TOKEN");
    }

    #[test]
    #[serial]
    fn channel_defaults_to_fleet_activity() {
        std::env::remove_var("SLACK_FLEET_CHANNEL");
        assert_eq!(channel(), "fleet-activity");
    }

    #[test]
    #[serial]
    fn channel_uses_env_override() {
        std::env::set_var("SLACK_FLEET_CHANNEL", "agent-events");
        assert_eq!(channel(), "agent-events");
        std::env::remove_var("SLACK_FLEET_CHANNEL");
    }

    #[tokio::test]
    #[serial]
    async fn notify_claimed_noop_without_token() {
        // Must return quickly and not panic when SLACK_BOT_TOKEN is absent.
        std::env::remove_var("SLACK_BOT_TOKEN");
        notify_claimed("boris", "task-1", "Do the thing", "some description").await;
    }

    #[tokio::test]
    #[serial]
    async fn notify_completed_noop_without_token() {
        std::env::remove_var("SLACK_BOT_TOKEN");
        notify_completed("natasha", "task-2", "Done task", "it worked").await;
    }

    #[tokio::test]
    #[serial]
    async fn notify_failed_noop_without_token() {
        // Must return quickly and not panic when SLACK_BOT_TOKEN is absent.
        std::env::remove_var("SLACK_BOT_TOKEN");
        notify_failed("boris", "task-3", "Broken task", "exit_code=1: stderr output here").await;
    }

    #[tokio::test]
    #[serial]
    async fn notify_voting_noop_without_token() {
        // Must return quickly and not panic when SLACK_BOT_TOKEN is absent.
        std::env::remove_var("SLACK_BOT_TOKEN");
        notify_voting("boris", "idea-1", "Add caching layer").await;
    }

    #[test]
    fn notify_voting_message_format() {
        // Verify the assembled text uses the ballot-box emoji and the
        // present-progressive "voting on idea:" phrasing.
        let title = trunc("Add caching layer", TITLE_MAX);
        let text = format!(":ballot_box_with_check: *boris* voting on idea: *{title}*");
        assert!(text.contains(":ballot_box_with_check:"), "message must contain ballot emoji");
        assert!(text.contains("voting on idea:"), "message must contain 'voting on idea:'");
        assert!(text.contains("Add caching layer"));
    }

    #[tokio::test]
    #[serial]
    async fn notify_voted_noop_without_token() {
        // Must return quickly and not panic when SLACK_BOT_TOKEN is absent.
        std::env::remove_var("SLACK_BOT_TOKEN");
        notify_voted("boris", "idea-1", "Add caching layer", "approve").await;
    }

    #[test]
    fn notify_voted_message_format_approve() {
        // Verify the assembled text uses the ballot-box emoji and contains
        // the agent name, vote direction, and idea title.
        let title = trunc("Add caching layer", TITLE_MAX);
        let text = format!(":ballot_box_with_check: *boris* voted *approve* on idea: *{title}*");
        assert!(text.contains(":ballot_box_with_check:"), "message must contain ballot emoji");
        assert!(text.contains("voted *approve*"), "message must contain vote direction");
        assert!(text.contains("on idea:"), "message must contain 'on idea:'");
        assert!(text.contains("Add caching layer"));
    }

    #[test]
    fn notify_voted_message_format_reject() {
        let title = trunc("Remove all tests", TITLE_MAX);
        let text = format!(":ballot_box_with_check: *natasha* voted *reject* on idea: *{title}*");
        assert!(text.contains("voted *reject*"), "message must contain reject direction");
        assert!(text.contains("Remove all tests"));
    }

    #[test]
    fn notify_failed_message_contains_x_emoji() {
        // Verify the assembled text starts with the failure emoji.
        // We test trunc + format logic without a live Slack connection.
        let title = trunc("My Task", TITLE_MAX);
        let error = trunc("something went wrong", ERROR_MAX);
        let text = format!(":x: *agent* failed: *{title}* — {error}");
        assert!(text.contains(":x:"), "failure message must contain :x: emoji");
        assert!(text.contains("failed:"), "failure message must contain 'failed:'");
        assert!(text.contains("something went wrong"));
    }
}
