//! Integration tests for `acc_agent::slack`.
//!
//! These tests exercise the public async API end-to-end — calling each
//! notification hook with representative inputs — and verify correct
//! no-op behaviour when `SLACK_BOT_TOKEN` is absent (the expected
//! production state for CI environments without Slack credentials).
//!
//! No live Slack connection is required; every test ensures the functions
//! return quickly and do not panic.
//!
//! # Coverage map
//!
//! | # | What is tested |
//! |---|---|
//! | 1  | `notify_claimed` is a no-op and returns without panic when token absent |
//! | 2  | `notify_completed` is a no-op and returns without panic when token absent |
//! | 3  | `notify_failed` is a no-op and returns without panic when token absent |
//! | 4  | `notify_voting` is a no-op and returns without panic when token absent |
//! | 5  | `notify_voted` with "approve" is a no-op and returns without panic when token absent |
//! | 6  | `notify_voted` with "reject" is a no-op and returns without panic when token absent |
//! | 7  | `notify_claimed` handles an empty description without panic |
//! | 8  | `notify_completed` handles an empty result string without panic |
//! | 9  | `notify_failed` handles an empty error string without panic |
//! | 10 | `notify_claimed` handles a very long title (> 80 chars) without panic |
//! | 11 | `notify_failed` handles a very long error string (> 160 chars) without panic |
//! | 12 | All five hooks can be called concurrently without panic or deadlock |

use serial_test::serial;

use acc_agent::slack::{
    notify_claimed, notify_completed, notify_failed, notify_voted, notify_voting,
};

// ── Test 1: notify_claimed no-op without token ────────────────────────────────

/// `notify_claimed` must return immediately without panic when
/// `SLACK_BOT_TOKEN` is not set.
#[tokio::test]
#[serial]
async fn test_01_notify_claimed_noop_without_token() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    notify_claimed("agent-boris", "task-001", "Implement feature X", "Full feature X implementation").await;
}

// ── Test 2: notify_completed no-op without token ──────────────────────────────

/// `notify_completed` must return immediately without panic when
/// `SLACK_BOT_TOKEN` is not set.
#[tokio::test]
#[serial]
async fn test_02_notify_completed_noop_without_token() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    notify_completed("agent-natasha", "task-002", "Fix bug #42", "Patched the off-by-one error").await;
}

// ── Test 3: notify_failed no-op without token ─────────────────────────────────

/// `notify_failed` must return immediately without panic when
/// `SLACK_BOT_TOKEN` is not set.
#[tokio::test]
#[serial]
async fn test_03_notify_failed_noop_without_token() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    notify_failed("agent-boris", "task-003", "Deploy service", "exit_code=1: port already in use").await;
}

// ── Test 4: notify_voting no-op without token ─────────────────────────────────

/// `notify_voting` must return immediately without panic when
/// `SLACK_BOT_TOKEN` is not set.
#[tokio::test]
#[serial]
async fn test_04_notify_voting_noop_without_token() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    notify_voting("agent-natasha", "idea-004", "Add distributed caching layer").await;
}

// ── Test 5: notify_voted (approve) no-op without token ───────────────────────

/// `notify_voted` with vote = "approve" must return immediately without panic
/// when `SLACK_BOT_TOKEN` is not set.
#[tokio::test]
#[serial]
async fn test_05_notify_voted_approve_noop_without_token() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    notify_voted("agent-boris", "idea-005", "Switch to async I/O everywhere", "approve").await;
}

// ── Test 6: notify_voted (reject) no-op without token ────────────────────────

/// `notify_voted` with vote = "reject" must return immediately without panic
/// when `SLACK_BOT_TOKEN` is not set.
#[tokio::test]
#[serial]
async fn test_06_notify_voted_reject_noop_without_token() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    notify_voted("agent-natasha", "idea-006", "Remove all tests", "reject").await;
}

// ── Test 7: notify_claimed with empty description ─────────────────────────────

/// `notify_claimed` must handle an empty description string gracefully
/// without panic.
#[tokio::test]
#[serial]
async fn test_07_notify_claimed_empty_description() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    // Empty description — the desc_part of the message should be omitted.
    notify_claimed("agent-boris", "task-007", "Quick fix", "").await;
}

// ── Test 8: notify_completed with empty result ────────────────────────────────

/// `notify_completed` must handle an empty result string gracefully without
/// panic.
#[tokio::test]
#[serial]
async fn test_08_notify_completed_empty_result() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    // Empty result — the result_part of the message should be omitted.
    notify_completed("agent-natasha", "task-008", "Silent success", "").await;
}

// ── Test 9: notify_failed with empty error ────────────────────────────────────

/// `notify_failed` must handle an empty error string gracefully without panic.
#[tokio::test]
#[serial]
async fn test_09_notify_failed_empty_error() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    // Empty error string — the error_part of the message should be omitted.
    notify_failed("agent-boris", "task-009", "Mysterious failure", "").await;
}

// ── Test 10: notify_claimed with very long title ──────────────────────────────

/// `notify_claimed` must not panic when title exceeds the 80-character
/// truncation limit.
#[tokio::test]
#[serial]
async fn test_10_notify_claimed_long_title() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    let long_title = "A".repeat(200);
    notify_claimed("agent-boris", "task-010", &long_title, "some description").await;
}

// ── Test 11: notify_failed with very long error ───────────────────────────────

/// `notify_failed` must not panic when the error string exceeds the 160-character
/// truncation limit.
#[tokio::test]
#[serial]
async fn test_11_notify_failed_long_error() {
    std::env::remove_var("SLACK_BOT_TOKEN");
    let long_error = "error: ".repeat(50); // 350 chars, well above 160 limit
    notify_failed("agent-natasha", "task-011", "Failing task", &long_error).await;
}

// ── Test 12: concurrent calls do not panic or deadlock ────────────────────────

/// All five notification hooks must be safely callable from concurrent async
/// tasks at the same time — no mutex poisoning, no deadlocks, no panics.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial]
async fn test_12_concurrent_hooks_do_not_panic() {
    std::env::remove_var("SLACK_BOT_TOKEN");

    let h1 = tokio::spawn(async {
        notify_claimed("agent-a", "task-c1", "Concurrent claim 1", "desc1").await;
    });
    let h2 = tokio::spawn(async {
        notify_completed("agent-b", "task-c2", "Concurrent complete 2", "result2").await;
    });
    let h3 = tokio::spawn(async {
        notify_failed("agent-c", "task-c3", "Concurrent failure 3", "err3").await;
    });
    let h4 = tokio::spawn(async {
        notify_voting("agent-d", "idea-c4", "Concurrent vote idea 4").await;
    });
    let h5 = tokio::spawn(async {
        notify_voted("agent-e", "idea-c5", "Concurrent voted idea 5", "approve").await;
    });

    // All handles must complete without error.
    for h in [h1, h2, h3, h4, h5] {
        h.await.expect("spawned slack-hook task must not panic");
    }
}
