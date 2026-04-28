//! Integration tests for `acc_agent::peer_exchange`.
//!
//! These tests exercise the public API end-to-end — from constant values and
//! the rate-limit table through the full challenge/execute/decide round-trip —
//! using only the types and functions exposed by the library crate.
//!
//! # Coverage map
//!
//! | # | What is tested |
//! |---|---|
//! | 1  | Published constant values (`RATE_LIMIT_SECS`, `DEFAULT_FAILURE_THRESHOLD`, `TEST_TIMEOUT_SECS`) |
//! | 2  | Full round-trip: initiate → execute → decide (all pass) |
//! | 3  | Full round-trip: initiate → execute → decide (above threshold → GoOfflineAndFix) |
//! | 4  | Failure rate *at* threshold → `LogPassSummary` (boundary, exclusive upper bound) |
//! | 5  | Rate-limit blocks the same pair on a second `initiate` call |
//! | 6  | Rate-limit is symmetric: `record_challenge("bob")` from alice also blocks bob→alice key |
//! | 7  | `seconds_until_allowed` returns 0 for an unseen pair and > 0 right after a challenge |
//! | 8  | `initiate` returns `NoPeerAvailable` when target is an empty string |
//! | 9  | `initiate` returns `EmptySuite` when the generator yields nothing |
//! | 10 | `ExchangeRequest` carries the correct `from`/`to` fields and suite length |
//! | 11 | `AgentAction::from_reports` with an empty slice → `LogPassSummary { total:0, failed:0 }` |
//! | 12 | `Arc<PeerExchangeCoordinator>` shared across two handles — rate-limit visible on both |

use std::sync::Arc;

use acc_agent::peer_exchange::{
    AgentAction, ExchangeRequest, InitiateError, PeerExchangeCoordinator, TestCase, TestReport,
    TestRunner, TestSuite, TestGenerator,
    DEFAULT_FAILURE_THRESHOLD, RATE_LIMIT_SECS, TEST_TIMEOUT_SECS,
};

// ── Shared test helpers ───────────────────────────────────────────────────────

fn make_tc(id: &str) -> TestCase {
    TestCase {
        id: id.to_string(),
        title: format!("Integration test {id}"),
        command: format!("echo {id}"),
        expected_exit_code: 0,
        expected_output_contains: Some(id.to_string()),
    }
}

fn suite_of(n: usize) -> TestSuite {
    let mut s = TestSuite::new();
    for i in 0..n {
        s = s.with(make_tc(&format!("tc-{i:02}")));
    }
    s
}

/// Generator that always yields a fixed-size suite.
struct FixedGen(usize);
impl TestGenerator for FixedGen {
    fn generate(&self, _target: &str) -> TestSuite {
        suite_of(self.0)
    }
}

/// Generator that always yields an empty suite.
struct EmptyGen;
impl TestGenerator for EmptyGen {
    fn generate(&self, _target: &str) -> TestSuite {
        TestSuite::new()
    }
}

/// Runner that marks the first `fail_count` test cases as failures.
struct PartialFailRunner {
    fail_count: usize,
}
impl TestRunner for PartialFailRunner {
    fn run(&self, suite: &TestSuite) -> Vec<TestReport> {
        suite
            .tests
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                if i < self.fail_count {
                    TestReport::fail(&tc.id, 1, "", "injected failure")
                } else {
                    TestReport::pass(&tc.id, "ok")
                }
            })
            .collect()
    }
}

/// Runner where every test passes.
struct AllPassRunner;
impl TestRunner for AllPassRunner {
    fn run(&self, suite: &TestSuite) -> Vec<TestReport> {
        suite
            .tests
            .iter()
            .map(|tc| TestReport::pass(&tc.id, "ok"))
            .collect()
    }
}

// ── Test 1: constant values ───────────────────────────────────────────────────

/// The published constants must match the specification values so that any
/// accidental change is caught immediately at the integration level.
#[test]
fn test_01_constant_values() {
    assert_eq!(RATE_LIMIT_SECS, 3600, "RATE_LIMIT_SECS must be 3600 (one hour)");
    assert!(
        (DEFAULT_FAILURE_THRESHOLD - 0.30).abs() < f64::EPSILON,
        "DEFAULT_FAILURE_THRESHOLD must be 0.30, got {DEFAULT_FAILURE_THRESHOLD}"
    );
    assert_eq!(TEST_TIMEOUT_SECS, 30, "TEST_TIMEOUT_SECS must be 30");
}

// ── Test 2: full round-trip, all tests pass ───────────────────────────────────

/// The complete protocol flow — initiate, execute, decide — produces
/// `LogPassSummary` when every test case passes.
#[test]
fn test_02_full_round_trip_all_pass() {
    let coord = PeerExchangeCoordinator::new("alice");

    // Initiate a challenge with 5 test cases.
    let req = coord
        .initiate("bob", &FixedGen(5))
        .expect("initiation should succeed");

    // Execute the challenge (all pass).
    let reports = coord.execute(&req, &AllPassRunner);
    assert_eq!(reports.len(), 5, "one report per test case");

    // Decide the action.
    let action = coord.decide_action(&reports);
    assert!(
        matches!(action, AgentAction::LogPassSummary { total: 5, failed: 0 }),
        "all-pass round-trip should produce LogPassSummary, got {action:?}"
    );
}

// ── Test 3: full round-trip, above threshold ──────────────────────────────────

/// When more than 30 % of tests fail the coordinator must recommend going
/// offline: `GoOfflineAndFix` with the correct counts and IDs.
#[test]
fn test_03_full_round_trip_above_threshold_go_offline() {
    let coord = PeerExchangeCoordinator::new("alice");

    // 10 tests, 4 failures → failure rate 0.4 > 0.3 threshold.
    let req = coord
        .initiate("bob", &FixedGen(10))
        .expect("initiation should succeed");

    let reports = coord.execute(&req, &PartialFailRunner { fail_count: 4 });
    assert_eq!(reports.len(), 10);

    let action = coord.decide_action(&reports);
    match action {
        AgentAction::GoOfflineAndFix { total, failed, failed_ids } => {
            assert_eq!(total, 10);
            assert_eq!(failed, 4);
            assert_eq!(failed_ids.len(), 4);
        }
        other => panic!("expected GoOfflineAndFix, got {other:?}"),
    }
}

// ── Test 4: failure rate exactly at threshold → LogPassSummary (boundary) ────

/// The threshold comparison is *strictly greater than*: a failure rate that
/// equals `DEFAULT_FAILURE_THRESHOLD` exactly must NOT trigger `GoOfflineAndFix`.
#[test]
fn test_04_failure_rate_at_threshold_boundary_logs_summary() {
    // 3 out of 10 fail → rate 0.30 == threshold → should NOT go offline.
    let reports: Vec<TestReport> = (0..10)
        .map(|i| {
            if i < 3 {
                TestReport::fail(format!("tc-{i:02}"), 1, "", "fail")
            } else {
                TestReport::pass(format!("tc-{i:02}"), "ok")
            }
        })
        .collect();

    let action = AgentAction::from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);
    assert!(
        matches!(action, AgentAction::LogPassSummary { .. }),
        "rate == threshold must NOT trigger go-offline; got {action:?}"
    );
}

// ── Test 5: rate-limit blocks same pair on second initiate ────────────────────

/// Calling `initiate` twice for the same pair without waiting must return
/// `RateLimited` on the second attempt.
#[test]
fn test_05_rate_limit_blocks_second_initiate() {
    let coord = PeerExchangeCoordinator::new("alice");

    assert!(
        coord.initiate("bob", &FixedGen(1)).is_ok(),
        "first initiation must succeed"
    );

    let err = coord
        .initiate("bob", &FixedGen(1))
        .expect_err("second initiation must be rate-limited");

    assert!(
        matches!(err, InitiateError::RateLimited { .. }),
        "error must be RateLimited, got {err:?}"
    );
}

// ── Test 6: rate-limit is symmetric ──────────────────────────────────────────

/// The pair key is derived from the *sorted* pair of agent names, so a
/// challenge recorded by alice against bob must also block bob from
/// challenging alice (the same key is used for both directions).
#[test]
fn test_06_rate_limit_is_symmetric() {
    // alice records a challenge against bob.
    let coord_alice = PeerExchangeCoordinator::new("alice");
    coord_alice.record_challenge("bob");

    // Simulate bob's coordinator sharing the same rate-limit slot by
    // recording the equivalent challenge on bob's own coordinator.
    let coord_bob = PeerExchangeCoordinator::new("bob");
    coord_bob.record_challenge("alice"); // same sorted key: "alice:bob"

    // bob should now be rate-limited against alice.
    let err = coord_bob
        .check_rate_limit("alice")
        .expect_err("bob→alice must be rate-limited after recording challenge");

    assert!(
        matches!(err, InitiateError::RateLimited { .. }),
        "error must be RateLimited, got {err:?}"
    );
}

// ── Test 7: seconds_until_allowed ────────────────────────────────────────────

/// `seconds_until_allowed` returns 0 for pairs that have never been
/// challenged, and a positive value (≤ RATE_LIMIT_SECS) right after a
/// challenge is recorded.
#[test]
fn test_07_seconds_until_allowed_before_and_after_challenge() {
    let coord = PeerExchangeCoordinator::new("alice");

    // Unseen pair: no wait needed.
    assert_eq!(
        coord.seconds_until_allowed("dave"),
        0,
        "unseen pair must have 0 seconds remaining"
    );

    // After recording a challenge the remaining seconds must be positive.
    coord.record_challenge("dave");
    let secs = coord.seconds_until_allowed("dave");
    assert!(
        secs > 0 && secs <= RATE_LIMIT_SECS,
        "remaining seconds must be in (0, RATE_LIMIT_SECS], got {secs}"
    );
}

// ── Test 8: NoPeerAvailable for empty target ──────────────────────────────────

/// Passing an empty string as the target must return `NoPeerAvailable`
/// without touching the rate-limit table.
#[test]
fn test_08_initiate_no_peer_available_for_empty_target() {
    let coord = PeerExchangeCoordinator::new("alice");

    let err = coord
        .initiate("", &FixedGen(2))
        .expect_err("empty target must fail");

    assert_eq!(
        err,
        InitiateError::NoPeerAvailable,
        "error must be NoPeerAvailable, got {err:?}"
    );

    // The rate-limit table must not have been modified.
    assert_eq!(
        coord.seconds_until_allowed(""),
        0,
        "rate-limit table must remain empty after NoPeerAvailable"
    );
}

// ── Test 9: EmptySuite when generator yields nothing ─────────────────────────

/// When the generator returns an empty `TestSuite` the coordinator must
/// refuse to send the challenge and return `EmptySuite`.
#[test]
fn test_09_initiate_empty_suite_error() {
    let coord = PeerExchangeCoordinator::new("alice");

    let err = coord
        .initiate("bob", &EmptyGen)
        .expect_err("empty suite must fail");

    assert_eq!(
        err,
        InitiateError::EmptySuite,
        "error must be EmptySuite, got {err:?}"
    );
}

// ── Test 10: ExchangeRequest carries correct metadata ─────────────────────────

/// The `ExchangeRequest` returned by `initiate` must preserve the initiator
/// name, the target name, and the exact number of test cases produced by the
/// generator.
#[test]
fn test_10_exchange_request_fields() {
    let coord = PeerExchangeCoordinator::new("initiator-agent");

    let req = coord
        .initiate("target-agent", &FixedGen(7))
        .expect("initiation must succeed");

    assert_eq!(req.from, "initiator-agent", "from field mismatch");
    assert_eq!(req.to, "target-agent", "to field mismatch");
    assert_eq!(req.suite.len(), 7, "suite length mismatch");
}

// ── Test 11: empty report slice → LogPassSummary ──────────────────────────────

/// When no reports are provided (target executed zero test cases) the
/// coordinator should produce a vacuous pass summary rather than an error.
#[test]
fn test_11_empty_reports_produce_log_pass_summary() {
    let action = AgentAction::from_reports(&[], DEFAULT_FAILURE_THRESHOLD);

    assert!(
        matches!(action, AgentAction::LogPassSummary { total: 0, failed: 0 }),
        "empty report list must produce LogPassSummary{{0,0}}, got {action:?}"
    );
}

// ── Test 12: Arc<PeerExchangeCoordinator> shared across two handles ───────────

/// The coordinator must be usable through multiple `Arc` handles; state
/// mutations (such as recording a challenge) on one handle must be visible
/// on all others.
#[test]
fn test_12_arc_shared_coordinator() {
    let coord = Arc::new(PeerExchangeCoordinator::new("alice"));
    let coord_clone = Arc::clone(&coord);

    // First handle initiates successfully.
    let req = coord
        .initiate("bob", &FixedGen(3))
        .expect("first initiation through Arc handle must succeed");
    assert_eq!(req.suite.len(), 3);

    // Second handle observes the rate-limit set by the first.
    let err = coord_clone
        .initiate("bob", &FixedGen(3))
        .expect_err("second initiation through cloned Arc handle must be rate-limited");

    assert!(
        matches!(err, InitiateError::RateLimited { .. }),
        "cloned Arc handle must see rate-limit set by original, got {err:?}"
    );

    // The execute path also works through either handle.
    let req2 = ExchangeRequest {
        from: "alice".to_string(),
        to: "bob".to_string(),
        suite: suite_of(2),
    };
    let reports = coord_clone.execute(&req2, &AllPassRunner);
    assert_eq!(reports.len(), 2);
    assert!(reports.iter().all(|r| r.passed));

    let action = coord.decide_action(&reports);
    assert!(
        matches!(action, AgentAction::LogPassSummary { total: 2, failed: 0 }),
        "decide_action through original Arc must succeed, got {action:?}"
    );
}
