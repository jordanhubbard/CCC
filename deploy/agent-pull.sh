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

log "Starting pull -> ${WORKSPACE}"
cd "${WORKSPACE}"
git fetch origin --quiet 2>>"${LOG_FILE}"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"

# Capture old HEAD before merge so we can detect what changed.
PREV_HEAD="$(git rev-parse HEAD)"
git merge --ff-only "origin/${BRANCH}" --quiet 2>>"${LOG_FILE}"
NEW_HEAD="$(git rev-parse HEAD)"
log "Pull complete ($(git rev-parse --short HEAD))"

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
