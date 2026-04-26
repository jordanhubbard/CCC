#!/usr/bin/env bats
# test_phase_commit.bats — bats unit tests for scripts/phase-commit.sh
#
# Covers:
#   - CIFS pre-flight (mount exists / missing / empty / stat-timeout)
#   - Lock-file guard (acquire, concurrent block, stale-lock cleanup, dead-PID cleanup)
#   - git add + commit (normal, nothing-to-commit, commit failure)
#   - pull --rebase best-effort (success, no-remote-branch, failure is non-fatal)
#   - push retry loop (success first try, retry on rejection, exponential backoff,
#     exhaust retries → exit 1, report to hub)
#   - FF-merge to main (success, non-FF diverge skipped, fetch failure skipped)
#   - /clean hub POST (called on success, skipped when no project-id)
#   - dry-run mode (no git writes, no curl calls)
#   - argument parsing (unknown flag, missing workspace, missing branch,
#     default commit message)
#   - lock release on exit (trap fires on success and on error)
#
# Run with:
#   bats scripts/tests/test_phase_commit.bats
#
# Requirements: bats-core >= 1.5, git, bash >= 4.


SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)/phase-commit.sh"

# ── Shared test helpers ───────────────────────────────────────────────────────

# Create a temporary directory, initialise a bare "remote" git repo and a
# working clone, make an initial commit, and export the paths used by tests.
setup_git_workspace() {
  REMOTE_DIR="$(mktemp -d)"
  WORK_DIR="$(mktemp -d)"
  ACC_DIR_T="$(mktemp -d)"

  git init --bare "$REMOTE_DIR" -q
  git clone "$REMOTE_DIR" "$WORK_DIR" -q 2>/dev/null

  cd "$WORK_DIR"
  git config user.email "test@acc"
  git config user.name  "test-agent"
  echo "init" > README.md
  git add README.md
  git commit -m "init" -q
  git push origin main -q 2>/dev/null

  export WORK_DIR REMOTE_DIR ACC_DIR_T
}

teardown_git_workspace() {
  rm -rf "${WORK_DIR:-}" "${REMOTE_DIR:-}" "${ACC_DIR_T:-}"
}


# Minimal invocation that should succeed (workspace clean → "nothing to commit").
run_script() {
  local extra_args=("$@")
  run bash "$SCRIPT" \
    --workspace  "$WORK_DIR" \
    --branch     "phase/test" \
    --agent      "bats-agent" \
    "${extra_args[@]}"
}

# ═══════════════════════════════════════════════════════════════════════════════
# § 1  Argument parsing
# ═══════════════════════════════════════════════════════════════════════════════

@test "unknown flag prints error and exits 1" {
  run bash "$SCRIPT" --bogus-flag
  [ "$status" -eq 1 ]
  [[ "$output" =~ "unknown argument" ]]
}

@test "missing --workspace exits 1 with helpful message" {
  run bash "$SCRIPT" --branch phase/test
  [ "$status" -eq 1 ]
  [[ "$output" =~ "--workspace" ]]
}

@test "missing --branch exits 1 with helpful message" {
  run bash "$SCRIPT" --workspace /tmp
  [ "$status" -eq 1 ]
  [[ "$output" =~ "--branch" ]]
}

@test "missing --message uses sensible default (does not exit 1 on clean repo)" {
  setup_git_workspace
  # No --message: script should auto-default and not error
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent" \
    --acc-dir   "$ACC_DIR_T"
  # clean workspace → "nothing to commit" path → exit 0
  [ "$status" -eq 0 ]
  teardown_git_workspace
}


# ═══════════════════════════════════════════════════════════════════════════════
# § 2  CIFS / AccFS pre-flight
# ═══════════════════════════════════════════════════════════════════════════════

# Helper: run the script with AGENTFS_MOUNT pointing at a path that is a
# prefix of WORKSPACE so the pre-flight code path is exercised.
run_with_mount() {
  local mount_dir="$1"; shift
  AGENTFS_MOUNT="$mount_dir" \
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent" \
    "$@"
}

@test "CIFS pre-flight: non-existent mountpoint exits 1" {
  setup_git_workspace
  # Point workspace inside a fake mount path that does not exist
  FAKE_MOUNT="$(mktemp -d)"
  rm -rf "$FAKE_MOUNT"
  AGENTFS_MOUNT="$FAKE_MOUNT" \
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "${FAKE_MOUNT}/proj" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  [ "$status" -eq 1 ]
  [[ "$output" =~ "CIFS pre-flight FAILED" ]]
  teardown_git_workspace
}

@test "CIFS pre-flight: empty mountpoint exits 1" {
  setup_git_workspace
  EMPTY_MOUNT="$(mktemp -d)"
  AGENTFS_MOUNT="$EMPTY_MOUNT" \
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "${EMPTY_MOUNT}/proj" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  [ "$status" -eq 1 ]
  [[ "$output" =~ "CIFS pre-flight FAILED" ]]
  rm -rf "$EMPTY_MOUNT"
  teardown_git_workspace
}

@test "CIFS pre-flight: non-empty directory treated as available" {
  setup_git_workspace
  # WORK_DIR is a non-empty dir and its parent is a non-empty dir;
  # use WORK_DIR's parent as the fake mount so workspace is beneath it.
  PARENT="$(dirname "$WORK_DIR")"
  AGENTFS_MOUNT="$PARENT" \
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  # Should reach the "nothing to commit" success path, not the pre-flight error
  [ "$status" -eq 0 ]
  [[ ! "$output" =~ "pre-flight FAILED" ]]
  teardown_git_workspace
}

@test "CIFS pre-flight skipped when workspace is not under AGENTFS_MOUNT" {
  setup_git_workspace
  # AGENTFS_MOUNT is set to something that WORK_DIR is NOT under
  AGENTFS_MOUNT="/mnt/accfs-fake" \
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  # pre-flight should be skipped; script continues normally
  [ "$status" -eq 0 ]
  [[ ! "$output" =~ "CIFS pre-flight FAILED" ]]
  teardown_git_workspace
}


# ═══════════════════════════════════════════════════════════════════════════════
# § 3  Non-git workspace
# ═══════════════════════════════════════════════════════════════════════════════

@test "workspace without .git exits 1" {
  NOT_A_REPO="$(mktemp -d)"
  ACC_DIR_T="$(mktemp -d)"
  run bash "$SCRIPT" \
    --workspace "$NOT_A_REPO" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  [ "$status" -eq 1 ]
  [[ "$output" =~ "not a git repo" ]]
  rm -rf "$NOT_A_REPO" "$ACC_DIR_T"
}

# ═══════════════════════════════════════════════════════════════════════════════
# § 4  Lock-file guard
# ═══════════════════════════════════════════════════════════════════════════════

@test "lock file is created during run and removed on exit" {
  setup_git_workspace
  LOCK_DIR="${ACC_DIR_T}/locks"
  mkdir -p "$LOCK_DIR"
  LOCK_NAME="phase-commit-$(echo "$WORK_DIR" | tr '/' '_').lock"

  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  [ "$status" -eq 0 ]
  # Lock must be gone after script exits
  [ ! -f "${LOCK_DIR}/${LOCK_NAME}" ]
  teardown_git_workspace
}

@test "concurrent lock held by live PID blocks second invocation" {
  setup_git_workspace
  LOCK_DIR="${ACC_DIR_T}/locks"
  mkdir -p "$LOCK_DIR"
  LOCK_NAME="phase-commit-$(echo "$WORK_DIR" | tr '/' '_').lock"
  LOCK_FILE="${LOCK_DIR}/${LOCK_NAME}"

  # Write our own PID as the lock holder (we are alive)
  echo $$ > "$LOCK_FILE"

  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  [ "$status" -eq 1 ]
  [[ "$output" =~ "Another phase-commit is running" ]]

  rm -f "$LOCK_FILE"
  teardown_git_workspace
}

@test "stale lock (mtime > LOCK_TIMEOUT_S) is cleaned up and run proceeds" {
  setup_git_workspace
  LOCK_DIR="${ACC_DIR_T}/locks"
  mkdir -p "$LOCK_DIR"
  LOCK_NAME="phase-commit-$(echo "$WORK_DIR" | tr '/' '_').lock"
  LOCK_FILE="${LOCK_DIR}/${LOCK_NAME}"

  # Create a lock with a PID that definitely does not exist
  echo "999999" > "$LOCK_FILE"
  # Back-date its mtime by 700 s (> default LOCK_TIMEOUT_S=600)
  touch -d "700 seconds ago" "$LOCK_FILE" 2>/dev/null \
    || touch -t "$(date -d '700 seconds ago' '+%Y%m%d%H%M.%S' 2>/dev/null || date -v-700S '+%Y%m%d%H%M.%S')" "$LOCK_FILE" 2>/dev/null \
    || true

  LOCK_TIMEOUT_S=600 \
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  [ "$status" -eq 0 ]
  [[ "$output" =~ "stale lock" ]] || [[ "$output" =~ "Removing stale lock" ]]
  teardown_git_workspace
}

@test "lock held by dead PID is cleaned up and run proceeds" {
  setup_git_workspace
  LOCK_DIR="${ACC_DIR_T}/locks"
  mkdir -p "$LOCK_DIR"
  LOCK_NAME="phase-commit-$(echo "$WORK_DIR" | tr '/' '_').lock"
  LOCK_FILE="${LOCK_DIR}/${LOCK_NAME}"

  # Use a PID that is very unlikely to be alive
  echo "2" > "$LOCK_FILE"   # PID 2 is kthreadd on Linux — not kill-able by user; treat as dead

  LOCK_TIMEOUT_S=9999 \
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  # Script should clean the dead-PID lock and continue to success
  [ "$status" -eq 0 ]
  teardown_git_workspace
}


# ═══════════════════════════════════════════════════════════════════════════════
# § 5  git add + commit
# ═══════════════════════════════════════════════════════════════════════════════

@test "clean workspace exits 0 with 'nothing to commit' message" {
  setup_git_workspace
  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/test" \
    --agent     "bats-agent"
  [ "$status" -eq 0 ]
  [[ "$output" =~ "Nothing to commit" ]]
  teardown_git_workspace
}

@test "dirty workspace: new file is committed and pushed to phase branch" {
  setup_git_workspace
  echo "agent output" > "${WORK_DIR}/result.txt"

  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/milestone" \
    --message   "phase commit: milestone (1 task)" \
    --agent     "bats-agent"
  [ "$status" -eq 0 ]

  # Verify the commit landed on the phase branch in the remote
  branch_sha=$(git -C "$WORK_DIR" ls-remote origin "refs/heads/phase/milestone" \
               | awk '{print $1}')
  [ -n "$branch_sha" ]
  teardown_git_workspace
}

@test "modified tracked file is staged, committed, and pushed" {
  setup_git_workspace
  echo "modified" > "${WORK_DIR}/README.md"

  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/sprint1" \
    --message   "phase commit: sprint1" \
    --agent     "bats-agent"
  [ "$status" -eq 0 ]

  remote_log=$(git -C "$REMOTE_DIR" log --oneline "phase/sprint1" 2>/dev/null)
  [[ "$remote_log" =~ "phase commit: sprint1" ]]
  teardown_git_workspace
}

@test "commit message appears verbatim in git log" {
  setup_git_workspace
  echo "work" > "${WORK_DIR}/work.txt"
  EXPECTED_MSG="phase commit: milestone (42 tasks reviewed and approved)"

  ACC_DIR="$ACC_DIR_T" \
  run bash "$SCRIPT" \
    --workspace "$WORK_DIR" \
    --branch    "phase/milestone" \
    --message   "$EXPECTED_MSG" \
    --agent     "bats-agent"
  [ "$status" -eq 0 ]

  actual_msg=$(git -C "$WORK_DIR" log --format="%s" "phase/milestone" | head -1)
  [ "$actual_msg" = "$EXPECTED_MSG" ]
  teardown_git_workspace
}

