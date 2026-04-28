//! Peer agent test-exchange protocol.
//!
//! Any two agents in the fleet can declare a priority test exchange with each
//! other, rate-limited to at most **once per hour per pair** (pair key is the
//! two agent names sorted alphabetically so the constraint is symmetric).
//!
//! # Protocol flow
//!
//! ```text
//! initiator                               target
//!   │  agent.test_challenge  ──────────────►  │
//!   │                                         │  execute tests sequentially
//!   │  agent.test_submit     ◄──────────────  │
//!   │
//!   └─ tally results; if failure_rate > threshold → emit ordered actions:
//!        1. GoOffline          — quench the poll loop
//!        2. RaiseBeadsTask     — file one repair task per failed test
//!           (issued in priority-descending order: highest priority first)
//!        3. BeginAutoFix       — signal that automated remediation starts
//! ```
//!
//! # Types
//!
//! | Type | Role |
//! |---|---|
//! | [`TestCase`] | A single test specification sent by the initiator |
//! | [`TestSuite`] | An ordered collection of `TestCase`s |
//! | [`ExchangeRequest`] | The `agent.test_challenge` bus-message body |
//! | [`TestReport`] | Per-test execution result produced by the target |
//! | [`AgentAction`] | A single observable side-effect step emitted on failure |
//! | [`InitiateError`] | Errors that can occur when initiating a challenge |
//! | [`TestGenerator`] | Trait for types that can build a `TestSuite` |
//! | [`TestRunner`] | Trait for types that can execute a `TestSuite` |
//! | [`PeerExchangeCoordinator`] | Top-level coordinator; owns the rate-limit table |
//!
//! # Rate limiting
//!
//! The coordinator keeps an in-memory `HashMap<String, Instant>` keyed by the
//! sorted pair `"{a}:{b}"` where `a <= b` lexicographically. An attempt to
//! initiate before `RATE_LIMIT_SECS` have elapsed since the last challenge
//! returns [`InitiateError::RateLimited`] with the remaining seconds.
//!
//! # Failure threshold
//!
//! After the target submits results, the failure rate (`failed / total`) is
//! compared against `FAILURE_THRESHOLD` (default 0.30, overridable via the
//! `ACC_TEST_FAILURE_THRESHOLD` environment variable). Exceeding the threshold
//! produces the sequence `[GoOffline, RaiseBeadsTask { .. }, …, BeginAutoFix]`;
//! staying at or below produces `[LogPassSummary { .. }]`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

// ── Public constants ──────────────────────────────────────────────────────────

/// Minimum gap between challenges for the same agent pair (seconds).
pub const RATE_LIMIT_SECS: u64 = 3600;

/// Default failure-rate threshold above which the agent goes offline.
pub const DEFAULT_FAILURE_THRESHOLD: f64 = 0.30;

/// Per-test execution timeout.
pub const TEST_TIMEOUT_SECS: u64 = 30;

// ── TestCase ──────────────────────────────────────────────────────────────────

/// A single test specification authored by the initiating agent.
///
/// `command` is a shell command run inside the challenged agent's workspace.
/// `expected_exit_code` defaults to `0`. `expected_output_contains` is an
/// optional substring that must appear in the combined stdout of the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCase {
    /// Stable short identifier (e.g. `"tc-01"`).
    pub id: String,
    /// Human-readable title shown in reports.
    pub title: String,
    /// Shell command executed by the target agent.
    pub command: String,
    /// Exit code the command must produce to pass (default `0`).
    #[serde(default)]
    pub expected_exit_code: i32,
    /// Optional stdout substring that must be present for the test to pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output_contains: Option<String>,
}

// ── TestSuite ─────────────────────────────────────────────────────────────────

/// An ordered collection of [`TestCase`]s sent as a single challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TestSuite {
    /// All test cases in execution order.
    pub tests: Vec<TestCase>,
}

impl TestSuite {
    /// Create an empty suite.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a test case and return `self` for chaining.
    pub fn with(mut self, tc: TestCase) -> Self {
        self.tests.push(tc);
        self
    }

    /// Number of test cases in this suite.
    pub fn len(&self) -> usize {
        self.tests.len()
    }

    /// Whether the suite contains no test cases.
    pub fn is_empty(&self) -> bool {
        self.tests.is_empty()
    }
}

// ── ExchangeRequest ───────────────────────────────────────────────────────────

/// Body of the `agent.test_challenge` bus message sent from initiator → target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeRequest {
    /// Agent name of the initiator.
    pub from: String,
    /// Agent name of the challenged target.
    pub to: String,
    /// The test suite the target must execute.
    pub suite: TestSuite,
}

// ── TestReport ────────────────────────────────────────────────────────────────

/// Execution result for a single [`TestCase`], produced by the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestReport {
    /// Matches [`TestCase::id`].
    pub test_id: String,
    /// `true` when the test satisfied all pass criteria.
    pub passed: bool,
    /// Actual exit code produced by the command.
    pub actual_exit_code: i32,
    /// Combined stdout captured from the command.
    pub stdout: String,
    /// Combined stderr captured from the command.
    pub stderr: String,
}

impl TestReport {
    /// Construct a passing report.
    pub fn pass(test_id: impl Into<String>, stdout: impl Into<String>) -> Self {
        TestReport {
            test_id: test_id.into(),
            passed: true,
            actual_exit_code: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// Construct a failing report.
    pub fn fail(
        test_id: impl Into<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        TestReport {
            test_id: test_id.into(),
            passed: false,
            actual_exit_code: exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }
}

// ── AgentAction ───────────────────────────────────────────────────────────────

/// A single observable side-effect step emitted after tallying test results.
///
/// When the failure rate exceeds the threshold, the coordinator produces an
/// **ordered sequence** of three actions that callers MUST execute in the order
/// they are yielded.  Each action is a distinct bus-observable event:
///
/// 1. [`AgentAction::GoOffline`]       — quench the poll loop (stop accepting work)
/// 2. [`AgentAction::RaiseBeadsTask`]  — file one repair task for a single failed
///    test; emitted once per failed test case in **priority-descending order**
///    (highest priority = lowest numeric value first, matching fleet task
///    conventions where `0` is most urgent)
/// 3. [`AgentAction::BeginAutoFix`]    — signal that automated remediation starts
///
/// [`AgentAction::GoOfflineAndFix`] is a convenience consolidated variant that
/// combines all three steps above into a single action, returned by
/// [`AgentAction::from_reports`] and [`PeerExchangeCoordinator::decide_action`]
/// for callers that do not need fine-grained step sequencing.
///
/// When the failure rate stays at or below the threshold the sequence contains
/// only a single [`AgentAction::LogPassSummary`] step.
///
/// Use [`AgentAction::sequence_from_reports`] to obtain the full ordered list
/// rather than constructing variants directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentAction {
    /// All tests passed (or failure rate ≤ threshold): log a pass summary.
    LogPassSummary {
        total: usize,
        failed: usize,
    },
    /// Step 1 of 3 — quench the poll loop so no new tasks are picked up.
    GoOffline {
        total: usize,
        failed: usize,
        /// IDs of all failed test cases (carried so observers have full context
        /// from the very first event).
        failed_ids: Vec<String>,
    },
    /// Step 2 of 3 — raise one repair (beads) task for a single failed test.
    ///
    /// Emitted once per failed test in priority-descending order.
    RaiseBeadsTask {
        /// ID of the specific test case that failed.
        test_id: String,
        /// Filing priority for this repair task (lower number = higher urgency).
        /// Tasks are raised from highest priority (0) to lowest so the most
        /// critical defects enter the queue first.
        priority: u32,
    },
    /// Step 3 of 3 — automated remediation begins; callers should start the
    /// fix pipeline.
    BeginAutoFix {
        total: usize,
        failed: usize,
        /// IDs of all failed test cases that triggered remediation.
        failed_ids: Vec<String>,
    },
    /// Consolidated failure action: go offline, raise beads tasks for all
    /// failed tests (priority-ordered), and begin auto-fix — all in one
    /// observable event.
    ///
    /// This variant is returned by [`AgentAction::from_reports`] and
    /// [`PeerExchangeCoordinator::decide_action`] for callers that want a
    /// single representative outcome rather than the full step sequence.
    GoOfflineAndFix {
        total: usize,
        failed: usize,
        /// IDs of every failed test case, in priority-descending order
        /// (index 0 = highest priority / most urgent defect).
        failed_ids: Vec<String>,
    },
}

impl AgentAction {
    /// Build the full ordered sequence of actions for the given reports and
    /// threshold.
    ///
    /// `threshold` is the *exclusive* upper bound on failure rate: a rate
    /// strictly greater than `threshold` triggers the three-step
    /// `[GoOffline, RaiseBeadsTask×N, BeginAutoFix]` sequence.
    ///
    /// Priorities for [`AgentAction::RaiseBeadsTask`] steps are assigned in
    /// descending order (highest priority = `0` first, incrementing by `1` for
    /// each subsequent failed test) so the most critical defect enters the
    /// queue first.
    pub fn sequence_from_reports(reports: &[TestReport], threshold: f64) -> Vec<Self> {
        let total = reports.len();
        if total == 0 {
            return vec![AgentAction::LogPassSummary { total: 0, failed: 0 }];
        }

        let failed_reports: Vec<&TestReport> = reports.iter().filter(|r| !r.passed).collect();
        let failed = failed_reports.len();
        let rate = failed as f64 / total as f64;

        if rate <= threshold {
            return vec![AgentAction::LogPassSummary { total, failed }];
        }

        let failed_ids: Vec<String> = failed_reports.iter().map(|r| r.test_id.clone()).collect();

        // Step 1 — go offline
        let mut sequence = vec![AgentAction::GoOffline {
            total,
            failed,
            failed_ids: failed_ids.clone(),
        }];

        // Step 2 — one RaiseBeadsTask per failed test, priority-descending
        // (priority 0 is most urgent; we issue it first so it enters the queue
        // with the highest urgency).
        for (priority, report) in failed_reports.iter().enumerate() {
            sequence.push(AgentAction::RaiseBeadsTask {
                test_id: report.test_id.clone(),
                priority: priority as u32,
            });
        }

        // Step 3 — begin auto-fix
        sequence.push(AgentAction::BeginAutoFix {
            total,
            failed,
            failed_ids,
        });

        sequence
    }

    /// Produce a single consolidated outcome for callers that need one
    /// representative action instead of the full step sequence.
    ///
    /// * When the failure rate is **at or below** `threshold` → [`AgentAction::LogPassSummary`].
    /// * When the failure rate **exceeds** `threshold` → [`AgentAction::GoOfflineAndFix`],
    ///   which bundles the total, failed count, and ordered failed-test IDs
    ///   into one action.
    ///
    /// Callers that need the full ordered
    /// `[GoOffline, RaiseBeadsTask×N, BeginAutoFix]` sequence should use
    /// [`AgentAction::sequence_from_reports`] instead.
    pub fn from_reports(reports: &[TestReport], threshold: f64) -> Self {
        let total = reports.len();
        if total == 0 {
            return AgentAction::LogPassSummary { total: 0, failed: 0 };
        }

        let failed_reports: Vec<&TestReport> = reports.iter().filter(|r| !r.passed).collect();
        let failed = failed_reports.len();
        let rate = failed as f64 / total as f64;

        if rate <= threshold {
            return AgentAction::LogPassSummary { total, failed };
        }

        let failed_ids: Vec<String> = failed_reports.iter().map(|r| r.test_id.clone()).collect();
        AgentAction::GoOfflineAndFix { total, failed, failed_ids }
    }

    /// Total tests involved (passed + failed).
    pub fn total(&self) -> usize {
        match self {
            AgentAction::LogPassSummary { total, .. } => *total,
            AgentAction::GoOffline { total, .. } => *total,
            AgentAction::RaiseBeadsTask { .. } => 0,
            AgentAction::BeginAutoFix { total, .. } => *total,
            AgentAction::GoOfflineAndFix { total, .. } => *total,
        }
    }

    /// Number of failed tests (0 for `RaiseBeadsTask` which is per-test).
    pub fn failed(&self) -> usize {
        match self {
            AgentAction::LogPassSummary { failed, .. } => *failed,
            AgentAction::GoOffline { failed, .. } => *failed,
            AgentAction::RaiseBeadsTask { .. } => 0,
            AgentAction::BeginAutoFix { failed, .. } => *failed,
            AgentAction::GoOfflineAndFix { failed, .. } => *failed,
        }
    }
}

// ── InitiateError ─────────────────────────────────────────────────────────────

/// Errors that can occur when an agent attempts to initiate a test challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InitiateError {
    /// The pair was challenged less than `RATE_LIMIT_SECS` ago.
    RateLimited {
        /// Seconds remaining until the next challenge is allowed.
        retry_after_seconds: u64,
    },
    /// No online peer was found to challenge.
    NoPeerAvailable,
    /// The generated suite was empty; nothing to send.
    EmptySuite,
    /// A network or server error prevented the challenge from being sent.
    SendFailed(String),
}

impl std::fmt::Display for InitiateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitiateError::RateLimited { retry_after_seconds } => {
                write!(f, "rate limited — retry in {retry_after_seconds}s")
            }
            InitiateError::NoPeerAvailable => write!(f, "no peer available"),
            InitiateError::EmptySuite => write!(f, "generated suite is empty"),
            InitiateError::SendFailed(msg) => write!(f, "send failed: {msg}"),
        }
    }
}

impl std::error::Error for InitiateError {}

// ── TestGenerator trait ───────────────────────────────────────────────────────

/// Trait for types that can generate a [`TestSuite`] targeting a specific peer.
pub trait TestGenerator {
    /// Produce a suite of tests for the given `target_agent`.
    ///
    /// Implementations may inspect the local workspace, capabilities, or any
    /// other context they hold. Returning an empty `TestSuite` is allowed but
    /// will cause initiation to fail with [`InitiateError::EmptySuite`].
    fn generate(&self, target_agent: &str) -> TestSuite;
}

// ── TestRunner trait ──────────────────────────────────────────────────────────

/// Trait for types that can execute a [`TestSuite`] and return reports.
pub trait TestRunner {
    /// Execute every test in `suite` and return one [`TestReport`] per case.
    ///
    /// Implementations MUST return exactly one report per test case in
    /// the same order. Each test case has a `TEST_TIMEOUT_SECS` bound on
    /// execution; timed-out tests should be recorded as failures.
    fn run(&self, suite: &TestSuite) -> Vec<TestReport>;
}

// ── PeerExchangeCoordinator ───────────────────────────────────────────────────

/// Pair key: two agent names sorted alphabetically, joined by `:`.
fn pair_key(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}:{b}")
    } else {
        format!("{b}:{a}")
    }
}

/// Read the failure threshold from the environment or fall back to the default.
pub fn failure_threshold() -> f64 {
    std::env::var("ACC_TEST_FAILURE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_FAILURE_THRESHOLD)
}

/// Top-level coordinator for the peer test-exchange protocol.
///
/// Holds the in-memory rate-limit table and delegates test generation and
/// execution to injected `TestGenerator`/`TestRunner` implementations.
///
/// The coordinator is intended to be wrapped in an `Arc<PeerExchangeCoordinator>`
/// so it can be shared across async tasks without additional synchronisation.
pub struct PeerExchangeCoordinator {
    /// `pair_key(a, b)` → time of the last challenge between a and b.
    rate_limits: Mutex<HashMap<String, Instant>>,
    /// Name of this agent (the initiator).
    pub agent_name: String,
}

impl PeerExchangeCoordinator {
    /// Create a new coordinator for `agent_name`.
    pub fn new(agent_name: impl Into<String>) -> Self {
        PeerExchangeCoordinator {
            rate_limits: Mutex::new(HashMap::new()),
            agent_name: agent_name.into(),
        }
    }

    // ── Rate-limit helpers ────────────────────────────────────────────────

    /// Return `Ok(())` if the pair `(self_name, target)` may initiate now, or
    /// `Err(InitiateError::RateLimited { retry_after_seconds })` if not.
    pub fn check_rate_limit(&self, target: &str) -> Result<(), InitiateError> {
        let key = pair_key(&self.agent_name, target);
        let limits = self.rate_limits.lock().expect("rate_limits poisoned");
        if let Some(&last) = limits.get(&key) {
            let elapsed = last.elapsed().as_secs();
            if elapsed < RATE_LIMIT_SECS {
                return Err(InitiateError::RateLimited {
                    retry_after_seconds: RATE_LIMIT_SECS - elapsed,
                });
            }
        }
        Ok(())
    }

    /// Record that a challenge between `self_name` and `target` happened now.
    pub fn record_challenge(&self, target: &str) {
        let key = pair_key(&self.agent_name, target);
        self.rate_limits
            .lock()
            .expect("rate_limits poisoned")
            .insert(key, Instant::now());
    }

    /// Seconds remaining until the pair `(self_name, target)` can challenge
    /// again, or `0` if no rate limit is currently active.
    pub fn seconds_until_allowed(&self, target: &str) -> u64 {
        let key = pair_key(&self.agent_name, target);
        let limits = self.rate_limits.lock().expect("rate_limits poisoned");
        if let Some(&last) = limits.get(&key) {
            let elapsed = last.elapsed().as_secs();
            if elapsed < RATE_LIMIT_SECS {
                return RATE_LIMIT_SECS - elapsed;
            }
        }
        0
    }

    // ── Challenge lifecycle ───────────────────────────────────────────────

    /// Attempt to initiate a challenge against `target` using the provided
    /// generator.
    ///
    /// Returns the [`ExchangeRequest`] on success (the caller is responsible
    /// for sending it over the bus), or an [`InitiateError`] describing why
    /// initiation could not proceed.
    pub fn initiate(
        &self,
        target: &str,
        generator: &dyn TestGenerator,
    ) -> Result<ExchangeRequest, InitiateError> {
        if target.is_empty() {
            return Err(InitiateError::NoPeerAvailable);
        }
        self.check_rate_limit(target)?;
        let suite = generator.generate(target);
        if suite.is_empty() {
            return Err(InitiateError::EmptySuite);
        }
        self.record_challenge(target);
        Ok(ExchangeRequest {
            from: self.agent_name.clone(),
            to: target.to_string(),
            suite,
        })
    }

    /// Execute the received [`ExchangeRequest`] using the provided runner and
    /// return one [`TestReport`] per test case.
    pub fn execute(
        &self,
        request: &ExchangeRequest,
        runner: &dyn TestRunner,
    ) -> Vec<TestReport> {
        runner.run(&request.suite)
    }

    /// Determine what ordered sequence of actions to take based on test reports
    /// and the current failure threshold.
    ///
    /// Returns the full [`AgentAction`] sequence as produced by
    /// [`AgentAction::sequence_from_reports`].  Callers must execute the
    /// actions **in order**: `GoOffline` → `RaiseBeadsTask×N` → `BeginAutoFix`,
    /// each emitted as a separate observable bus event.
    pub fn decide_actions(&self, reports: &[TestReport]) -> Vec<AgentAction> {
        AgentAction::sequence_from_reports(reports, failure_threshold())
    }

    /// Determine a single consolidated outcome action for the given test reports.
    ///
    /// Returns [`AgentAction::LogPassSummary`] when the failure rate is at or
    /// below the threshold, or [`AgentAction::GoOfflineAndFix`] when it exceeds
    /// it.  This is the single-action companion to [`decide_actions`]; use the
    /// latter when you need the full step-by-step sequence.
    pub fn decide_action(&self, reports: &[TestReport]) -> AgentAction {
        AgentAction::from_reports(reports, failure_threshold())
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_tc(id: &str) -> TestCase {
        TestCase {
            id: id.to_string(),
            title: format!("Test {id}"),
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

    fn all_pass(suite: &TestSuite) -> Vec<TestReport> {
        suite
            .tests
            .iter()
            .map(|tc| TestReport::pass(&tc.id, "ok"))
            .collect()
    }

    fn reports_with_failures(total: usize, fail_count: usize) -> Vec<TestReport> {
        let mut reports = Vec::new();
        for i in 0..total {
            if i < fail_count {
                reports.push(TestReport::fail(
                    format!("tc-{i:02}"),
                    1,
                    "",
                    "command not found",
                ));
            } else {
                reports.push(TestReport::pass(format!("tc-{i:02}"), "ok"));
            }
        }
        reports
    }

    // ── 1. pair_key is symmetric ──────────────────────────────────────────

    #[test]
    fn test_pair_key_is_symmetric() {
        assert_eq!(pair_key("alpha", "beta"), pair_key("beta", "alpha"));
    }

    // ── 2. pair_key uses alphabetical order ───────────────────────────────

    #[test]
    fn test_pair_key_alphabetical_order() {
        let k = pair_key("zebra", "ant");
        assert!(k.starts_with("ant:"), "smaller name must be first: {k}");
    }

    // ── 3. check_rate_limit passes when table is empty ────────────────────

    #[test]
    fn test_check_rate_limit_allows_fresh_pair() {
        let coord = PeerExchangeCoordinator::new("alice");
        assert!(coord.check_rate_limit("bob").is_ok());
    }

    // ── 4. check_rate_limit blocks immediately after record ───────────────

    #[test]
    fn test_check_rate_limit_blocks_after_record() {
        let coord = PeerExchangeCoordinator::new("alice");
        coord.record_challenge("bob");
        let err = coord.check_rate_limit("bob").unwrap_err();
        assert!(matches!(err, InitiateError::RateLimited { .. }));
    }

    // ── 5. seconds_until_allowed is 0 for unseen pair ─────────────────────

    #[test]
    fn test_seconds_until_allowed_zero_for_fresh() {
        let coord = PeerExchangeCoordinator::new("alice");
        assert_eq!(coord.seconds_until_allowed("bob"), 0);
    }

    // ── 6. seconds_until_allowed is near RATE_LIMIT_SECS right after record

    #[test]
    fn test_seconds_until_allowed_near_limit_after_record() {
        let coord = PeerExchangeCoordinator::new("alice");
        coord.record_challenge("carol");
        let secs = coord.seconds_until_allowed("carol");
        // Should be very close to RATE_LIMIT_SECS (within 2s of test execution).
        assert!(secs > 0 && secs <= RATE_LIMIT_SECS, "got {secs}");
    }

    // ── 7. rate limit is symmetric (bob→alice same as alice→bob) ─────────

    #[test]
    fn test_rate_limit_symmetry() {
        // Use a coordinator "from bob's perspective"
        let coord_alice = PeerExchangeCoordinator::new("alice");
        coord_alice.record_challenge("bob");

        // Now check from bob's perspective using the same key
        let coord_bob = PeerExchangeCoordinator::new("bob");
        // Manually insert the same key to simulate a shared table
        {
            let key = pair_key("alice", "bob");
            coord_bob
                .rate_limits
                .lock()
                .unwrap()
                .insert(key, Instant::now());
        }
        let err = coord_bob.check_rate_limit("alice").unwrap_err();
        assert!(matches!(err, InitiateError::RateLimited { .. }));
    }

    // ── 8. initiate succeeds with a non-empty suite ───────────────────────

    #[test]
    fn test_initiate_succeeds_with_nonempty_suite() {
        struct Gen;
        impl TestGenerator for Gen {
            fn generate(&self, _: &str) -> TestSuite {
                suite_of(3)
            }
        }

        let coord = PeerExchangeCoordinator::new("alice");
        let req = coord.initiate("bob", &Gen).unwrap();
        assert_eq!(req.from, "alice");
        assert_eq!(req.to, "bob");
        assert_eq!(req.suite.len(), 3);
    }

    // ── 9. initiate returns EmptySuite when generator gives nothing ───────

    #[test]
    fn test_initiate_returns_empty_suite_error() {
        struct EmptyGen;
        impl TestGenerator for EmptyGen {
            fn generate(&self, _: &str) -> TestSuite {
                TestSuite::new()
            }
        }

        let coord = PeerExchangeCoordinator::new("alice");
        let err = coord.initiate("bob", &EmptyGen).unwrap_err();
        assert_eq!(err, InitiateError::EmptySuite);
    }

    // ── 10. initiate returns NoPeerAvailable for empty target ─────────────

    #[test]
    fn test_initiate_no_peer_available() {
        struct Gen;
        impl TestGenerator for Gen {
            fn generate(&self, _: &str) -> TestSuite {
                suite_of(1)
            }
        }

        let coord = PeerExchangeCoordinator::new("alice");
        let err = coord.initiate("", &Gen).unwrap_err();
        assert_eq!(err, InitiateError::NoPeerAvailable);
    }

    // ── 11. initiate returns RateLimited on second call ───────────────────

    #[test]
    fn test_initiate_rate_limited_on_second_call() {
        struct Gen;
        impl TestGenerator for Gen {
            fn generate(&self, _: &str) -> TestSuite {
                suite_of(1)
            }
        }

        let coord = PeerExchangeCoordinator::new("alice");
        assert!(coord.initiate("bob", &Gen).is_ok());
        let err = coord.initiate("bob", &Gen).unwrap_err();
        assert!(matches!(err, InitiateError::RateLimited { .. }));
    }

    // ── 12. execute delegates to runner and returns all reports ───────────

    #[test]
    fn test_execute_returns_one_report_per_case() {
        struct EchoRunner;
        impl TestRunner for EchoRunner {
            fn run(&self, suite: &TestSuite) -> Vec<TestReport> {
                all_pass(suite)
            }
        }

        let coord = PeerExchangeCoordinator::new("bob");
        let suite = suite_of(4);
        let req = ExchangeRequest {
            from: "alice".to_string(),
            to: "bob".to_string(),
            suite,
        };
        let reports = coord.execute(&req, &EchoRunner);
        assert_eq!(reports.len(), 4);
        assert!(reports.iter().all(|r| r.passed));
    }

    // ── 13. AgentAction::from_reports — all pass → LogPassSummary ─────────

    #[test]
    fn test_action_all_pass_logs_summary() {
        let reports = reports_with_failures(5, 0);
        let action = AgentAction::from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);
        assert!(matches!(action, AgentAction::LogPassSummary { total: 5, failed: 0 }));
    }

    // ── 14. AgentAction::from_reports — rate > threshold → GoOfflineAndFix ──

    #[test]
    fn test_action_above_threshold_go_offline_and_fix() {
        // 4 out of 10 fail → rate 0.4 > 0.3 threshold
        // from_reports returns GoOfflineAndFix (consolidated single action).
        let reports = reports_with_failures(10, 4);
        let action = AgentAction::from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);
        match action {
            AgentAction::GoOfflineAndFix { total, failed, failed_ids } => {
                assert_eq!(total, 10);
                assert_eq!(failed, 4);
                assert_eq!(failed_ids.len(), 4);
            }
            other => panic!("expected GoOfflineAndFix, got {other:?}"),
        }
    }

    // ── 15. AgentAction::from_reports — rate == threshold → LogPassSummary

    #[test]
    fn test_action_at_threshold_logs_summary() {
        // Exactly 30% fail (3/10) → rate is NOT strictly > threshold → pass
        let reports = reports_with_failures(10, 3);
        let action = AgentAction::from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);
        assert!(
            matches!(action, AgentAction::LogPassSummary { .. }),
            "rate == threshold should NOT trigger go-offline: {action:?}"
        );
    }

    // ── 16. AgentAction::from_reports — empty list → LogPassSummary ───────

    #[test]
    fn test_action_empty_reports_logs_summary() {
        let action = AgentAction::from_reports(&[], DEFAULT_FAILURE_THRESHOLD);
        assert!(matches!(action, AgentAction::LogPassSummary { total: 0, failed: 0 }));
    }

    // ── 17. Coordinator can be wrapped in Arc and shared ──────────────────

    #[test]
    fn test_coordinator_arc_shared() {
        struct FixedGen(usize);
        impl TestGenerator for FixedGen {
            fn generate(&self, _: &str) -> TestSuite {
                suite_of(self.0)
            }
        }

        let coord = Arc::new(PeerExchangeCoordinator::new("alice"));
        let coord2 = Arc::clone(&coord);

        // First initiation from one Arc handle succeeds.
        let req = coord.initiate("bob", &FixedGen(2)).unwrap();
        assert_eq!(req.suite.len(), 2);

        // Rate-limit is visible from the second Arc handle.
        let err = coord2.initiate("bob", &FixedGen(2)).unwrap_err();
        assert!(matches!(err, InitiateError::RateLimited { .. }));
    }

    // ── 18. sequence_from_reports — pass produces single LogPassSummary ───

    #[test]
    fn test_sequence_all_pass_is_single_log_summary() {
        let reports = reports_with_failures(5, 0);
        let seq = AgentAction::sequence_from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);
        assert_eq!(seq.len(), 1);
        assert!(matches!(seq[0], AgentAction::LogPassSummary { total: 5, failed: 0 }));
    }

    // ── 19. sequence_from_reports — failure produces 3-step ordered sequence

    #[test]
    fn test_sequence_above_threshold_has_three_step_structure() {
        // 4 failures out of 10 → rate 0.4 > 0.3 threshold
        // Expected: [GoOffline, RaiseBeadsTask×4, BeginAutoFix]  (length 6)
        let reports = reports_with_failures(10, 4);
        let seq = AgentAction::sequence_from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);

        // Total length: 1 (GoOffline) + 4 (RaiseBeadsTask) + 1 (BeginAutoFix)
        assert_eq!(seq.len(), 6, "sequence length must be 1 + failed_count + 1");

        // Step 1 must be GoOffline
        match &seq[0] {
            AgentAction::GoOffline { total, failed, failed_ids } => {
                assert_eq!(*total, 10);
                assert_eq!(*failed, 4);
                assert_eq!(failed_ids.len(), 4);
            }
            other => panic!("first step must be GoOffline, got {other:?}"),
        }

        // Steps 2..5 must be RaiseBeadsTask with ascending priority values
        for (i, step) in seq[1..5].iter().enumerate() {
            match step {
                AgentAction::RaiseBeadsTask { test_id, priority } => {
                    assert_eq!(*priority, i as u32,
                        "RaiseBeadsTask priority must be sequential (0-based) got {priority}");
                    assert!(!test_id.is_empty(), "test_id must not be empty");
                }
                other => panic!("step {i} must be RaiseBeadsTask, got {other:?}"),
            }
        }

        // Last step must be BeginAutoFix
        match &seq[5] {
            AgentAction::BeginAutoFix { total, failed, failed_ids } => {
                assert_eq!(*total, 10);
                assert_eq!(*failed, 4);
                assert_eq!(failed_ids.len(), 4);
            }
            other => panic!("last step must be BeginAutoFix, got {other:?}"),
        }
    }

    // ── 20. sequence_from_reports — first RaiseBeadsTask has priority 0 ───

    #[test]
    fn test_sequence_first_raise_beads_task_has_highest_priority() {
        // The first RaiseBeadsTask must have priority 0 (most urgent) so the
        // highest-priority defect enters the queue first.
        let reports = reports_with_failures(5, 2);
        let seq = AgentAction::sequence_from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);
        // Structure: [GoOffline, RaiseBeadsTask(0), RaiseBeadsTask(1), BeginAutoFix]
        assert_eq!(seq.len(), 4);
        match &seq[1] {
            AgentAction::RaiseBeadsTask { priority, .. } => {
                assert_eq!(*priority, 0, "first RaiseBeadsTask must have priority 0");
            }
            other => panic!("expected RaiseBeadsTask at index 1, got {other:?}"),
        }
        match &seq[2] {
            AgentAction::RaiseBeadsTask { priority, .. } => {
                assert_eq!(*priority, 1, "second RaiseBeadsTask must have priority 1");
            }
            other => panic!("expected RaiseBeadsTask at index 2, got {other:?}"),
        }
    }

    // ── 21. sequence_from_reports — GoOffline failed_ids match RaiseBeadsTask

    #[test]
    fn test_sequence_failed_ids_consistent_across_steps() {
        let reports = reports_with_failures(6, 3);
        let seq = AgentAction::sequence_from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);

        // Collect IDs from GoOffline
        let offline_ids: Vec<String> = match &seq[0] {
            AgentAction::GoOffline { failed_ids, .. } => failed_ids.clone(),
            other => panic!("expected GoOffline, got {other:?}"),
        };

        // Collect IDs from all RaiseBeadsTask steps
        let raise_ids: Vec<String> = seq[1..seq.len() - 1]
            .iter()
            .map(|s| match s {
                AgentAction::RaiseBeadsTask { test_id, .. } => test_id.clone(),
                other => panic!("expected RaiseBeadsTask, got {other:?}"),
            })
            .collect();

        // Collect IDs from BeginAutoFix
        let fix_ids: Vec<String> = match seq.last().unwrap() {
            AgentAction::BeginAutoFix { failed_ids, .. } => failed_ids.clone(),
            other => panic!("expected BeginAutoFix, got {other:?}"),
        };

        assert_eq!(offline_ids, raise_ids,
            "GoOffline.failed_ids must match RaiseBeadsTask test_ids in order");
        assert_eq!(offline_ids, fix_ids,
            "GoOffline.failed_ids must match BeginAutoFix.failed_ids");
    }

    // ── 22. sequence_from_reports — single failure produces minimal sequence

    #[test]
    fn test_sequence_single_failure_minimal_structure() {
        // 1 failure out of 1 total → rate 1.0 > threshold
        // Sequence: [GoOffline, RaiseBeadsTask(0), BeginAutoFix]
        let reports = vec![TestReport::fail("tc-00", 1, "", "command not found")];
        let seq = AgentAction::sequence_from_reports(&reports, DEFAULT_FAILURE_THRESHOLD);
        assert_eq!(seq.len(), 3);
        assert!(matches!(seq[0], AgentAction::GoOffline { .. }));
        assert!(matches!(seq[1], AgentAction::RaiseBeadsTask { priority: 0, .. }));
        assert!(matches!(seq[2], AgentAction::BeginAutoFix { .. }));
    }

    // ── 23. sequence_from_reports — empty reports → single LogPassSummary ─

    #[test]
    fn test_sequence_empty_reports_is_pass_summary() {
        let seq = AgentAction::sequence_from_reports(&[], DEFAULT_FAILURE_THRESHOLD);
        assert_eq!(seq.len(), 1);
        assert!(matches!(seq[0], AgentAction::LogPassSummary { total: 0, failed: 0 }));
    }

    // ── 24. decide_actions on coordinator returns full sequence ────────────

    #[test]
    fn test_coordinator_decide_actions_returns_sequence() {
        let coord = PeerExchangeCoordinator::new("alice");
        let reports = reports_with_failures(10, 4); // 40% > 30% threshold
        let seq = coord.decide_actions(&reports);
        // Must have GoOffline first and BeginAutoFix last
        assert!(matches!(seq.first().unwrap(), AgentAction::GoOffline { .. }),
            "first action must be GoOffline");
        assert!(matches!(seq.last().unwrap(), AgentAction::BeginAutoFix { .. }),
            "last action must be BeginAutoFix");
        // RaiseBeadsTask steps in the middle
        let raise_count = seq.iter().filter(|a| matches!(a, AgentAction::RaiseBeadsTask { .. })).count();
        assert_eq!(raise_count, 4, "one RaiseBeadsTask per failed test");
    }
}
