#!/usr/bin/env bash
# agent-pull.sh — git pull, run migrations, then restart hermes if Python changed.
# Kept as a compatibility entry point for nodes that haven't yet
# updated acc-agent. Once all nodes run acc-agent >= the upgrade
# subcommand, this file can be deleted.
set -euo pipefail

ACC_DIR="${HOME}/.acc"
[[ -d "${HOME}/.acc" ]] || ACC_DIR="${HOME}/.ccc"
source "${ACC_DIR}/.env" 2>/dev/null || true
WORKSPACE="${ACC_DIR}/workspace"
LOG_FILE="${ACC_DIR}/logs/pull.log"
mkdir -p "${ACC_DIR}/logs"

log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] [agent-pull] $*" | tee -a "${LOG_FILE}"; }

# ── DNS / connectivity check + rocky-remote fallback ──────────────────────────
# On bullwinkle (jordan's home Mac) the NVIDIA-internal 10.x DNS servers are
# unreachable from home.  Detect the failure early and transparently switch the
# workspace remote to rocky over Tailscale so git fetch works without any
# manual intervention.
#
# ROCKY_REMOTE: Tailscale address of the rocky hub (set in ~/.acc/.env or here)
ROCKY_REMOTE="${ROCKY_GIT_REMOTE:-jkh@100.89.199.14:Src/ACC}"
ROCKY_REMOTE_NAME="rocky"

dns_ok() {
    # Returns 0 if we can resolve github.com, 1 otherwise.
    # Uses a 5-second timeout so we don't stall for 30s like cargo does.
    if command -v nslookup >/dev/null 2>&1; then
        nslookup -timeout=5 github.com >/dev/null 2>&1
    elif command -v host >/dev/null 2>&1; then
        host -W 5 github.com >/dev/null 2>&1
    elif command -v dig >/dev/null 2>&1; then
        dig +time=5 +tries=1 github.com >/dev/null 2>&1
    else
        # No DNS tools — try a TCP connect to github.com:443 with a short timeout
        curl -sf --connect-timeout 5 --max-time 5 https://github.com >/dev/null 2>&1
    fi
}

ensure_good_remote() {
    local repo_dir="$1"
    cd "${repo_dir}"

    if dns_ok; then
        log "DNS OK — using origin (github.com)"
        # If we previously switched to rocky, offer to switch back but don't
        # break anything: leave origin pointing at github for future use.
        return 0
    fi

    log "WARNING: DNS resolution for github.com failed — origin fetch will time out"

    # Check whether rocky remote is already set up
    if git remote get-url "${ROCKY_REMOTE_NAME}" >/dev/null 2>&1; then
        log "rocky remote already configured ($(git remote get-url ${ROCKY_REMOTE_NAME}))"
    else
        log "Adding git remote '${ROCKY_REMOTE_NAME}' → ${ROCKY_REMOTE}"
        git remote add "${ROCKY_REMOTE_NAME}" "${ROCKY_REMOTE}"
    fi

    # Verify the rocky remote is still reachable (Tailscale must be up)
    if git ls-remote --heads "${ROCKY_REMOTE_NAME}" >/dev/null 2>&1; then
        log "rocky remote reachable — switching fetch to rocky"
        # Temporarily repoint origin so the rest of this script (which uses
        # 'origin') transparently fetches from rocky.
        local original_origin
        original_origin=$(git remote get-url origin)
        git remote set-url origin "${ROCKY_REMOTE}"
        log "origin temporarily → ${ROCKY_REMOTE} (was ${original_origin})"
        # Stash the original URL so fix-dns-bullwinkle.sh or a future pull can
        # restore it when DNS is healthy again.
        git config --local acc.origin-github-url "${original_origin}" 2>/dev/null || true
    else
        log "ERROR: rocky remote also unreachable (is Tailscale running? is rocky online?)" >&2
        log "       Tailscale IP: 100.89.199.14  Remote: ${ROCKY_REMOTE}" >&2
        log "       Proceeding with cached local state only — no pull will happen." >&2
        # Don't exit 1 here: the agent should still start with its existing
        # binary.  The build step in restart-agent.sh will handle the no-network
        # case separately.
        return 1
    fi
}

log "Starting pull -> ${WORKSPACE}"
cd "${WORKSPACE}"

# Run the DNS check + remote fixup before any network operation.
ensure_good_remote "${WORKSPACE}" || {
    log "Skipping git fetch — no usable remote available"
    # Still run migrations/upgrade against the existing local state.
    goto_migrations=true
}

if [[ "${goto_migrations:-false}" != "true" ]]; then
    git fetch origin --quiet 2>>"${LOG_FILE}"
    BRANCH="$(git rev-parse --abbrev-ref HEAD)"

    # Capture old HEAD before merge so we can detect what changed.
    PREV_HEAD="$(git rev-parse HEAD)"
    git merge --ff-only "origin/${BRANCH}" --quiet 2>>"${LOG_FILE}"
    NEW_HEAD="$(git rev-parse HEAD)"
    log "Pull complete ($(git rev-parse --short HEAD))"
else
    BRANCH="$(git rev-parse --abbrev-ref HEAD)"
    PREV_HEAD="$(git rev-parse HEAD)"
    NEW_HEAD="${PREV_HEAD}"
    log "Pull skipped — using local HEAD $(git rev-parse --short HEAD)"
fi

# ── Sync secondary clone (~/Src/ACC) if it exists ─────────────────────────────
# Hermes is installed as an editable package from ~/Src/ACC/hermes/ on most
# nodes (set up during initial bootstrap), but fleet updates land in
# ~/.acc/workspace/.  Without syncing both, deployed Python fixes don't reach
# the running hermes import path until a manual pull.
SRC_CLONE="${HOME}/Src/ACC"
if [[ -d "${SRC_CLONE}/.git" ]]; then
    log "Syncing secondary clone ${SRC_CLONE}"
    (
        cd "${SRC_CLONE}"
        # Apply the same DNS-aware remote logic to the secondary clone.
        ensure_good_remote "${SRC_CLONE}" || { log "Skipping secondary clone fetch"; exit 0; }
        git fetch origin --quiet 2>>"${LOG_FILE}"
        src_branch="$(git rev-parse --abbrev-ref HEAD)"
        git merge --ff-only "origin/${src_branch}" --quiet 2>>"${LOG_FILE}" \
            && log "Secondary clone synced ($(git rev-parse --short HEAD))" \
            || log "WARNING: secondary clone fast-forward failed — may need manual pull"
    )
fi

# Detect Python or shell-script changes that require a hermes restart.
NEEDS_RESTART="false"
if [[ "$PREV_HEAD" != "$NEW_HEAD" ]]; then
    if git diff --name-only "$PREV_HEAD" "$NEW_HEAD" | grep -qE '\.(py|sh)$'; then
        NEEDS_RESTART="true"
        log "Python/shell files changed — will restart supervisor after upgrade"
    fi
fi

# Run migrations (do NOT exec — we need to continue after this returns).
ACC_AGENT="${HOME}/.acc/bin/acc-agent"
[[ -x "${ACC_AGENT}" ]] || ACC_AGENT="$(command -v acc-agent 2>/dev/null || echo "")"
if [[ -n "${ACC_AGENT}" ]]; then
    log "Running acc-agent upgrade"
    "${ACC_AGENT}" upgrade "$@"
else
    log "WARNING: acc-agent not found -- running legacy run-migrations.sh fallback"
    bash "${WORKSPACE}/deploy/run-migrations.sh" 2>>"${LOG_FILE}" || true
fi

# ── Smoke-test hermes import before restarting ─────────────────────────────────
# Verify run_agent imports cleanly so a broken deploy never silently kills a
# running hermes.  Tests the venv Python against whichever source path hermes
# is installed from (editable installs use the live source file on disk).
HERMES_VENV="${ACC_DIR}/hermes-venv"
HERMES_PYTHON="${HERMES_VENV}/bin/python3"
[[ -x "${HERMES_PYTHON}" ]] || HERMES_PYTHON="$(command -v python3 2>/dev/null || true)"

if [[ -x "${HERMES_PYTHON}" ]] && [[ "$NEEDS_RESTART" == "true" ]]; then
    log "Smoke-testing hermes import..."
    if "${HERMES_PYTHON}" -c "from run_agent import AIAgent" 2>>"${LOG_FILE}"; then
        log "Smoke test passed — hermes import OK"
    else
        log "ERROR: hermes import failed after pull — aborting restart to protect running agent"
        log "       Fix the Python error in run_agent.py and re-run agent-pull.sh manually"
        exit 1
    fi
fi

# Signal the supervisor to gracefully restart all children (including hermes)
# if any Python or shell files changed.  The supervisor writes its PID to
# supervisor.pid; SIGUSR1 triggers a full stop-and-respawn of all children.
if [[ "$NEEDS_RESTART" == "true" ]]; then
    SUPERVISOR_PID_FILE="${ACC_DIR}/supervisor.pid"
    if [[ -f "${SUPERVISOR_PID_FILE}" ]]; then
        SUPERVISOR_PID="$(cat "${SUPERVISOR_PID_FILE}")"
        if kill -USR1 "$SUPERVISOR_PID" 2>/dev/null; then
            log "Sent SIGUSR1 to supervisor (pid ${SUPERVISOR_PID}) — hermes restarting"
        else
            log "WARNING: could not signal supervisor pid ${SUPERVISOR_PID} — manual restart may be needed"
        fi
    else
        log "WARNING: ${SUPERVISOR_PID_FILE} not found — cannot auto-restart hermes"
    fi
fi
