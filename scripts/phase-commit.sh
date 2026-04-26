#!/usr/bin/env bash
# phase-commit.sh — Commit & push AccFS workspace to a phase branch, then FF-merge to main.
#
# This is the shell-equivalent of the Rust execute_phase_commit_task / run_git_phase_commit /
# run_git_merge_to_main logic in agent/acc-agent/src/tasks.rs.  It is invoked by operators,
# cron jobs, or CI pipelines that need to run a phase commit outside the Rust agent binary
# (e.g. from a migration, a recovery script, or a one-shot deploy step).
#
# Lifecycle
# ─────────
#  1. CIFS pre-flight  — verify AccFS mount is healthy before touching git
#  2. Lock-file guard  — prevent concurrent phase-commits on the same workspace
#  3. Stale-lock cleanup — remove locks left by dead processes (> LOCK_TIMEOUT_S seconds old)
#  4. git add -A + commit
#  5. git fetch + merge --ff-only origin/<phase-branch>  (absorb concurrent pushes, no history rewrite)
#  6. git push origin <phase-branch>  with exponential-backoff retry
#  7. git fetch + checkout main + pull --ff-only + merge --ff-only <phase-branch> + push main
#  8. POST /api/projects/<id>/clean  to mark project AgentFS clean
#
# Usage
# ─────
#   bash scripts/phase-commit.sh \
#       --workspace  /path/to/project/workspace \
#       --branch     phase/milestone \
#       --message    "phase commit: milestone (42 tasks reviewed)" \
#       [--project-id <id>]   \
#       [--acc-url   <url>]   \
#       [--acc-token <tok>]   \
#       [--agent     <name>]  \
#       [--dry-run]
#
# Environment (all overridable by CLI flags)
# ─────────────────────────────────────────
#   WORKSPACE          path to the git working tree (AccFS project dir)
#   PHASE_BRANCH       target branch, e.g. "phase/milestone"
#   COMMIT_MSG         git commit message
#   PROJECT_ID         used for the /clean API call (optional)
#   ACC_URL            hub base URL, e.g. "http://rocky:8789"
#   ACC_TOKEN          bearer token for the hub API
#   AGENT_NAME         appears in git author and log lines
#   ACC_DIR            root of agent's state dir (default: ~/.acc)
#   AGENTFS_MOUNT      path of the CIFS/Samba mount point (default: ${ACC_DIR}/shared)
#   LOCK_TIMEOUT_S     seconds after which a lock is considered stale (default: 600)
#   PUSH_MAX_RETRIES   maximum push attempts before giving up (default: 5)
#   PUSH_RETRY_BASE_S  base sleep in seconds for exponential backoff (default: 4)
#   DRY_RUN            set to "1" to skip all writes

set -euo pipefail

# ── Argument parsing ──────────────────────────────────────────────────────────

WORKSPACE="${WORKSPACE:-}"
PHASE_BRANCH="${PHASE_BRANCH:-}"
COMMIT_MSG="${COMMIT_MSG:-}"
PROJECT_ID="${PROJECT_ID:-}"
ACC_URL="${ACC_URL:-}"
ACC_TOKEN="${ACC_TOKEN:-}"
AGENT_NAME="${AGENT_NAME:-unknown}"
ACC_DIR="${ACC_DIR:-${HOME}/.acc}"
AGENTFS_MOUNT="${AGENTFS_MOUNT:-}"
LOCK_TIMEOUT_S="${LOCK_TIMEOUT_S:-600}"
PUSH_MAX_RETRIES="${PUSH_MAX_RETRIES:-5}"
PUSH_RETRY_BASE_S="${PUSH_RETRY_BASE_S:-4}"
DRY_RUN="${DRY_RUN:-0}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --workspace)   WORKSPACE="$2";   shift 2 ;;
    --branch)      PHASE_BRANCH="$2"; shift 2 ;;
    --message)     COMMIT_MSG="$2";  shift 2 ;;
    --project-id)  PROJECT_ID="$2";  shift 2 ;;
    --acc-url)     ACC_URL="$2";     shift 2 ;;
    --acc-token)   ACC_TOKEN="$2";   shift 2 ;;
    --agent)       AGENT_NAME="$2";  shift 2 ;;
    --dry-run)     DRY_RUN=1;        shift   ;;
    *) echo "ERROR: unknown argument: $1" >&2; exit 1 ;;
  esac
done

# Load .env if present (lowest priority — CLI flags already set above)
ENV_FILE="${ACC_DIR}/.env"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
  # Re-apply defaults that may have been overwritten by the sourced file
  # only if the variable was not set by the caller (CLI already parsed above).
fi

# Apply defaults that depend on ACC_DIR (which may come from .env)
AGENTFS_MOUNT="${AGENTFS_MOUNT:-${ACC_DIR}/shared}"

# ── Validate required arguments ───────────────────────────────────────────────

if [[ -z "$WORKSPACE" ]]; then
  echo "ERROR: --workspace (or \$WORKSPACE) is required" >&2; exit 1
fi
if [[ -z "$PHASE_BRANCH" ]]; then
  echo "ERROR: --branch (or \$PHASE_BRANCH) is required" >&2; exit 1
fi
if [[ -z "$COMMIT_MSG" ]]; then
  COMMIT_MSG="phase commit: ${PHASE_BRANCH}"
fi

# ── Logging ───────────────────────────────────────────────────────────────────

LOG_DIR="${ACC_DIR}/logs"
LOG_FILE="${LOG_DIR}/phase-commit.log"
mkdir -p "$LOG_DIR"

log() {
  local msg="[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] [${AGENT_NAME}] [phase-commit] $1"
  echo "$msg" >&2
  echo "$msg" >> "$LOG_FILE"
}

# ── Helper: flatten multi-line git stderr to the most useful line ─────────────
# Mirrors flatten_stderr() in tasks.rs: prefer error:/fatal:/! [rejected]/
# failed-to lines over hint: lines, then fall back to first non-empty line.

flatten_stderr() {
  local input="$1"
  local line
  while IFS= read -r line; do
    local trimmed="${line#"${line%%[! ]*}"}"   # ltrim
    case "$trimmed" in
      error:*|fatal:*|"! [rejected]"*|"failed to"*)
        echo "$line"; return ;;
    esac
  done <<< "$input"
  # No diagnostic line — return first non-empty
  while IFS= read -r line; do
    [[ -n "${line// }" ]] && { echo "$line"; return; }
  done <<< "$input"
}

# ── Helper: POST to hub API (best-effort, no exit on failure) ─────────────────

hub_post() {
  local path="$1"
  local body="${2:-{}}"
  if [[ -z "$ACC_URL" ]]; then
    log "hub_post: ACC_URL not set — skipping $path"
    return 0
  fi
  local token_hdr=""
  [[ -n "$ACC_TOKEN" ]] && token_hdr="-H \"Authorization: Bearer ${ACC_TOKEN}\""
  if [[ "$DRY_RUN" == "1" ]]; then
    log "DRY RUN: POST ${ACC_URL}${path} body=${body}"
    return 0
  fi
  local http_code
  http_code=$(curl -sf -o /dev/null -w "%{http_code}" \
    -X POST \
    -H "Content-Type: application/json" \
    ${ACC_TOKEN:+-H "Authorization: Bearer ${ACC_TOKEN}"} \
    -d "$body" \
    --connect-timeout 10 --max-time 30 \
    "${ACC_URL}${path}" 2>/dev/null || echo "000")
  if [[ "$http_code" == 2* ]]; then
    log "hub_post ${path}: HTTP ${http_code}"
  else
    log "hub_post ${path}: HTTP ${http_code} (non-fatal)"
  fi
}

# ── Step 1: CIFS / AccFS pre-flight ──────────────────────────────────────────
# AccFS is a Samba/CIFS share mounted at ${AGENTFS_MOUNT}.  Before we touch
# git, confirm the mount is alive: (a) the mountpoint exists, (b) it is
# actually a mount (not a dangling directory), and (c) a simple stat succeeds
# within a timeout.  If any check fails we abort rather than committing to a
# stale or empty workspace — the classic "commit an empty directory" incident.

cifs_preflight() {
  local mount="$1"

  # (a) mountpoint must exist
  if [[ ! -d "$mount" ]]; then
    log "CIFS pre-flight FAILED: mountpoint does not exist: $mount"
    return 1
  fi

  # (b) must be a real mount (Linux: findmnt; macOS: mount -t smbfs/cifs)
  local is_mounted=0
  if command -v findmnt &>/dev/null; then
    findmnt --noheadings --target "$mount" &>/dev/null && is_mounted=1
  elif command -v mount &>/dev/null; then
    mount | grep -q " on ${mount} " && is_mounted=1
  fi
  # On macOS the check may differ; also accept if the workspace itself exists
  # under the mount (allows tests to pass with a plain directory).
  if [[ "$is_mounted" -eq 0 ]]; then
    # Graceful fallback: if the directory is non-empty treat it as mounted
    # (supports test environments without a real CIFS mount).
    if [[ -n "$(ls -A "$mount" 2>/dev/null)" ]]; then
      log "CIFS pre-flight: $mount not a mount-point but non-empty — treating as available"
      return 0
    fi
    log "CIFS pre-flight FAILED: $mount is not mounted and is empty"
    return 1
  fi

  # (c) stat must succeed (catches stale NFS-style hangs)
  local stat_ok=0
  if command -v timeout &>/dev/null; then
    timeout 10 ls "$mount" &>/dev/null && stat_ok=1
  else
    ls "$mount" &>/dev/null && stat_ok=1
  fi
  if [[ "$stat_ok" -eq 0 ]]; then
    log "CIFS pre-flight FAILED: stat on $mount timed out or failed (stale mount?)"
    return 1
  fi

  log "CIFS pre-flight OK: $mount"
  return 0
}

# Skip CIFS pre-flight when workspace lives outside the AccFS mount
# (e.g. during local dev or if AGENTFS_MOUNT is unset).
if [[ -n "$AGENTFS_MOUNT" ]] && [[ "$WORKSPACE" == "${AGENTFS_MOUNT}"* ]]; then
  if ! cifs_preflight "$AGENTFS_MOUNT"; then
    exit 1
  fi
fi

# ── Validate workspace is a git repo ─────────────────────────────────────────

if [[ ! -d "${WORKSPACE}/.git" ]]; then
  log "ERROR: workspace is not a git repo: $WORKSPACE"
  exit 1
fi

# ── Step 2 & 3: Lock-file guard + stale-lock cleanup ─────────────────────────
# Use a per-workspace lock file to serialise concurrent phase-commits.
# A lock is stale when its mtime is older than LOCK_TIMEOUT_S seconds
# (meaning the process that created it has died).

LOCK_DIR="${ACC_DIR}/locks"
mkdir -p "$LOCK_DIR"
LOCK_FILE="${LOCK_DIR}/phase-commit-$(echo "$WORKSPACE" | tr '/' '_').lock"

acquire_lock() {
  # Clean up a stale lock if one exists
  if [[ -f "$LOCK_FILE" ]]; then
    local lock_pid lock_age now
    lock_pid=$(cat "$LOCK_FILE" 2>/dev/null || echo "")
    now=$(date +%s)
    # mtime via stat (portable: try GNU then BSD)
    local mtime
    if stat --version &>/dev/null 2>&1; then
      mtime=$(stat -c %Y "$LOCK_FILE" 2>/dev/null || echo "$now")
    else
      mtime=$(stat -f %m "$LOCK_FILE" 2>/dev/null || echo "$now")
    fi
    lock_age=$(( now - mtime ))

    if [[ "$lock_age" -gt "$LOCK_TIMEOUT_S" ]]; then
      log "Removing stale lock (age=${lock_age}s pid=${lock_pid}): $LOCK_FILE"
      rm -f "$LOCK_FILE"
    else
      # Check if the PID is still alive
      if [[ -n "$lock_pid" ]] && kill -0 "$lock_pid" 2>/dev/null; then
        log "ERROR: Another phase-commit is running (pid=$lock_pid, age=${lock_age}s) — aborting"
        return 1
      else
        log "Lock held by dead process (pid=$lock_pid) — cleaning up"
        rm -f "$LOCK_FILE"
      fi
    fi
  fi

  echo $$ > "$LOCK_FILE"
  log "Lock acquired: $LOCK_FILE (pid=$$)"
  return 0
}

release_lock() {
  if [[ -f "$LOCK_FILE" ]]; then
    local held_by
    held_by=$(cat "$LOCK_FILE" 2>/dev/null || echo "")
    if [[ "$held_by" == "$$" ]]; then
      rm -f "$LOCK_FILE"
      log "Lock released: $LOCK_FILE"
    fi
  fi
}

# Release lock on exit (including error paths)
trap 'release_lock' EXIT

if ! acquire_lock; then
  exit 1
fi

# ── clear_stale_index_lock — remove stale .git/index.lock ────────────────────
# A stale index.lock (left by a killed git process) will cause every subsequent
# git operation in this workspace to fail with "Another git process seems to be
# running in this repository".  We remove it unconditionally here, BEFORE any
# git command runs AND before the script-delegation branch below, so that the
# guard fires regardless of whether we go inline or delegate to a workspace-
# local scripts/phase-commit.sh.
#
# Safety: we only remove the lock when it is older than LOCK_TIMEOUT_S seconds
# (same threshold used by the phase-commit concurrency lock above).  A fresh
# lock (age < LOCK_TIMEOUT_S) is left in place — it almost certainly belongs
# to a concurrently running git process and removing it would corrupt that
# process's index write.

clear_stale_index_lock() {
  local index_lock="${WORKSPACE}/.git/index.lock"
  if [[ ! -f "$index_lock" ]]; then
    return 0
  fi

  local now mtime lock_age
  now=$(date +%s)
  if stat --version &>/dev/null 2>&1; then
    mtime=$(stat -c %Y "$index_lock" 2>/dev/null || echo "$now")
  else
    mtime=$(stat -f %m "$index_lock" 2>/dev/null || echo "$now")
  fi
  lock_age=$(( now - mtime ))

  if [[ "$lock_age" -gt "$LOCK_TIMEOUT_S" ]]; then
    log "Removing stale index.lock (age=${lock_age}s): $index_lock"
    rm -f "$index_lock"
  else
    log "index.lock present but fresh (age=${lock_age}s < ${LOCK_TIMEOUT_S}s) — leaving in place"
  fi
}

clear_stale_index_lock

# ── Delegate to workspace-local phase-commit.sh when present ─────────────────
# If the project workspace ships its own scripts/phase-commit.sh we exec into
# it immediately so that its pre-flight guards (execute-bit assertion,
# --force-with-lease push, etc.) fire for every pipeline run — not just when
# the wrapper is called directly.
#
# The clear_stale_index_lock call above ensures the lock is already clean
# before we hand off, so the workspace-local script does not need to repeat
# that step (though it may do so harmlessly if it has its own guard).
#
# The lock acquired above is intentionally released before exec so the child
# process can manage its own concurrency state.  We pass through the subset of
# flags that scripts/phase-commit.sh understands; pipeline-internal options
# (--project-id, --acc-url, --acc-token, --agent) have no equivalent in the
# workspace script and are therefore omitted.
#
# This is the fix for task-61c762130aa34238b44a1d36d723f59b.

WORKSPACE_PHASE_COMMIT="${WORKSPACE}/scripts/phase-commit.sh"
if [[ -f "$WORKSPACE_PHASE_COMMIT" ]]; then
  log "Delegating to workspace-local scripts/phase-commit.sh (task-61c762130aa34238b44a1d36d723f59b fix)"
  release_lock

  _delegate_args=()
  [[ -n "$COMMIT_MSG" ]]   && _delegate_args+=("$COMMIT_MSG")
  [[ "$DRY_RUN" == "1" ]]  && _delegate_args+=("--skip-push")

  exec bash "$WORKSPACE_PHASE_COMMIT" "${_delegate_args[@]}"
  # exec replaces this process; the lines below are unreachable if exec succeeds.
  log "ERROR: exec into $WORKSPACE_PHASE_COMMIT failed"
  exit 1
fi

# ── Step 4: git add -A + commit ───────────────────────────────────────────────

cd "$WORKSPACE"

log "Checking out branch: $PHASE_BRANCH"
if [[ "$DRY_RUN" == "1" ]]; then
  log "DRY RUN: git checkout -B $PHASE_BRANCH"
else
  checkout_out=$(git checkout -B "$PHASE_BRANCH" 2>&1) || {
    log "ERROR: git checkout -B $PHASE_BRANCH: $(flatten_stderr "$checkout_out")"
    exit 1
  }
fi

log "Staging all changes"
if [[ "$DRY_RUN" == "1" ]]; then
  log "DRY RUN: git add -A"
else
  add_out=$(git add -A 2>&1) || {
    log "ERROR: git add -A: $(flatten_stderr "$add_out")"
    exit 1
  }
fi

# Check if there is anything to commit (nothing_to_commit is not an error)
if git diff --cached --quiet 2>/dev/null && git diff --quiet 2>/dev/null; then
  log "Nothing to commit in $WORKSPACE — workspace is clean"
  NOTHING_TO_COMMIT=1
else
  NOTHING_TO_COMMIT=0
fi

COMMIT_SHA=""
if [[ "$NOTHING_TO_COMMIT" -eq 0 ]]; then
  log "Committing: $COMMIT_MSG"
  if [[ "$DRY_RUN" == "1" ]]; then
    log "DRY RUN: git commit -m \"$COMMIT_MSG\""
    COMMIT_SHA="dry-run-sha"
  else
    commit_out=$(git \
      -c "user.email=${AGENT_NAME}@acc" \
      -c "user.name=${AGENT_NAME}" \
      commit -m "$COMMIT_MSG" 2>&1) || {
      # "nothing to commit" after add -A can race; treat as clean
      if echo "$commit_out" | grep -q "nothing to commit"; then
        log "Nothing to commit (race with add); workspace clean"
        NOTHING_TO_COMMIT=1
      else
        log "ERROR: git commit: $(flatten_stderr "$commit_out")"
        exit 1
      fi
    }
    if [[ "$NOTHING_TO_COMMIT" -eq 0 ]]; then
      COMMIT_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "?")
      log "Committed: $COMMIT_SHA"
    fi
  fi
fi

# ── Step 5: fetch + merge --ff-only (absorb concurrent remote pushes) ────────
# WHY NOT pull --rebase?
# The phase/<branch> branch is shared: multiple agents can push to it
# concurrently.  git pull --rebase rewrites local commit SHAs, which
# means the next agent that fetches will see a diverged history and
# itself produce a non-fast-forward rejection — the very failure we are
# trying to prevent (see docs/git-push-timeout-investigation.md, Incident 8,
# Mitigation H rev 2, and task-203f3e70a84c48be8d8c40dc9994ddfb).
#
# fetch + merge --ff-only is safe: it integrates remote-only advances
# without any history rewrite.  If local and remote have genuinely
# diverged (both sides independently committed), --ff-only fails fast
# with a clear error rather than silently rewriting history.
#
# Best-effort: the remote branch may not exist yet (first phase_commit
# ever).  Failure here is non-fatal; the push step below will surface
# the real error and the retry loop will handle it.

log "Syncing with origin/${PHASE_BRANCH} before push (fetch + merge --ff-only, best-effort)"
if [[ "$DRY_RUN" != "1" ]]; then
  if git fetch origin "$PHASE_BRANCH" --quiet 2>/dev/null; then
    remote_ref="origin/${PHASE_BRANCH}"
    if git rev-parse --verify "$remote_ref" &>/dev/null; then
      if git merge-base --is-ancestor "$remote_ref" HEAD 2>/dev/null; then
        log "Local branch is already ahead of remote — no merge needed"
      else
        merge_ff_out=$(git merge --ff-only "$remote_ref" --quiet 2>&1) || {
          log "Pre-push merge --ff-only skipped (non-fatal): $(flatten_stderr "$merge_ff_out")"
        }
      fi
    fi
  else
    log "Fetch skipped (non-fatal): remote branch may not exist yet"
  fi
fi

# ── Step 6: git push with exponential-backoff retry ──────────────────────────

REMOTE_URL=$(git remote get-url origin 2>/dev/null || echo "")
if [[ -z "$REMOTE_URL" ]]; then
  log "WARNING: no remote 'origin' configured — skipping push"
  PUSH_RESULT="committed locally (no remote)"
else
  push_attempt=0
  push_sleep="$PUSH_RETRY_BASE_S"
  PUSH_RESULT=""

  while [[ "$push_attempt" -lt "$PUSH_MAX_RETRIES" ]]; do
    push_attempt=$(( push_attempt + 1 ))
    log "Push attempt ${push_attempt}/${PUSH_MAX_RETRIES} → origin/${PHASE_BRANCH}"

    if [[ "$DRY_RUN" == "1" ]]; then
      log "DRY RUN: git push origin $PHASE_BRANCH"
      PUSH_RESULT="dry-run push"
      break
    fi

    push_out=$(git push origin "$PHASE_BRANCH" 2>&1) && {
      PUSH_RESULT="pushed origin/${PHASE_BRANCH} @ ${COMMIT_SHA:-HEAD}"
      log "Push succeeded: $PUSH_RESULT"
      break
    }

    push_err=$(flatten_stderr "$push_out")
    log "Push attempt ${push_attempt} failed: ${push_err}"

    if [[ "$push_attempt" -ge "$PUSH_MAX_RETRIES" ]]; then
      log "ERROR: push failed after ${PUSH_MAX_RETRIES} attempts: ${push_err}"
      # Report failure to hub (best-effort)
      if [[ -n "$PROJECT_ID" ]]; then
        hub_post "/api/projects/${PROJECT_ID}/phase-commit-failed" \
          "{\"reason\":\"push failed: ${push_err}\"}"
      fi
      exit 1
    fi

    # Exponential backoff with a cap of 60 s
    log "Retrying in ${push_sleep}s …"
    sleep "$push_sleep"
    push_sleep=$(( push_sleep * 2 ))
    [[ "$push_sleep" -gt 60 ]] && push_sleep=60

    # Re-fetch + merge --ff-only before next attempt (the rejection may have
    # been caused by a concurrent push from another agent; absorb it without
    # rewriting history)
    retry_fetch_out=$(git fetch origin "$PHASE_BRANCH" --quiet 2>&1) || true
    retry_ref="origin/${PHASE_BRANCH}"
    if git rev-parse --verify "$retry_ref" &>/dev/null 2>&1; then
      if ! git merge-base --is-ancestor "$retry_ref" HEAD 2>/dev/null; then
        retry_merge_out=$(git merge --ff-only "$retry_ref" --quiet 2>&1) || {
          log "Pre-retry merge --ff-only skipped: $(flatten_stderr "$retry_merge_out")"
        }
      fi
    fi
  done
fi

# ── Step 7: FF-merge phase branch → main ─────────────────────────────────────
# Only performed when the push succeeded and main can be fast-forwarded.
# Non-FF scenarios are left for human review; we never do octopus merges.

MERGE_RESULT="skipped"

if [[ "$DRY_RUN" != "1" ]] && [[ -n "$REMOTE_URL" ]]; then
  log "Attempting FF-merge of ${PHASE_BRANCH} → main"

  merge_failed() {
    log "FF-merge to main skipped/failed: $1"
    MERGE_RESULT="not merged: $1"
  }

  fetch_main_out=$(git fetch origin --quiet 2>&1) || {
    merge_failed "$(flatten_stderr "$fetch_main_out")"
    goto_mark=1
  }

  if [[ "${goto_mark:-0}" -eq 0 ]]; then
    checkout_main_out=$(git checkout main 2>&1) || {
      merge_failed "checkout main: $(flatten_stderr "$checkout_main_out")"
      goto_mark=1
    }
  fi

  if [[ "${goto_mark:-0}" -eq 0 ]]; then
    pull_main_out=$(git pull --ff-only origin main --quiet 2>&1) || {
      merge_failed "pull --ff-only main: $(flatten_stderr "$pull_main_out")"
      goto_mark=1
    }
  fi

  if [[ "${goto_mark:-0}" -eq 0 ]]; then
    merge_out=$(git merge --ff-only "$PHASE_BRANCH" --quiet 2>&1) || {
      merge_failed "merge --ff-only ${PHASE_BRANCH}: $(flatten_stderr "$merge_out")"
      goto_mark=1
    }
  fi

  if [[ "${goto_mark:-0}" -eq 0 ]]; then
    push_main_out=$(git push origin main 2>&1) || {
      merge_failed "push main: $(flatten_stderr "$push_main_out")"
      goto_mark=1
    }
    [[ "${goto_mark:-0}" -eq 0 ]] && MERGE_RESULT="fast-forwarded"
  fi

  log "FF-merge result: $MERGE_RESULT"
fi

# ── Step 8: POST /api/projects/<id>/clean ────────────────────────────────────

if [[ -n "$PROJECT_ID" ]] && [[ "$PUSH_RESULT" != "" ]]; then
  log "Marking project ${PROJECT_ID} as clean"
  hub_post "/api/projects/${PROJECT_ID}/clean" "{}"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

SUMMARY="push: ${PUSH_RESULT}; main: ${MERGE_RESULT}"
log "Done — $SUMMARY"
echo "$SUMMARY"
