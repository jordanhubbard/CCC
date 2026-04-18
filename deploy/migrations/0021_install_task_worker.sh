#!/usr/bin/env bash
# Description: Install acc-task-worker — fleet task executor daemon.
#
# Context: acc-agent gained a `tasks` subcommand that polls /api/tasks,
# atomically claims open fleet tasks, executes them in an AgentFS workspace
# via `claude -p`, and marks them complete. This replaces ad-hoc per-repo
# beads polling and the old pattern of agents fighting over git worktrees.
#
# New files in repo:
#   agent/acc-agent/src/tasks.rs
#   deploy/systemd/acc-task-worker.service
#   deploy/launchd/com.acc.task-worker.plist
#
# Condition: all agent nodes (linux + macos). Idempotent.

ACC_DEST="${HOME}/.acc"
[[ -d "$ACC_DEST" ]] || ACC_DEST="${HOME}/.ccc"
WORKSPACE="${ACC_DEST}/workspace"

m_info "Install acc-task-worker (fleet task executor)"

# ── Verify acc-agent binary supports the tasks subcommand ────────────────────
ACC_BIN="${ACC_DEST}/bin/acc-agent"
if [[ ! -x "$ACC_BIN" ]]; then
  m_warn "acc-agent binary not found at ${ACC_BIN} — run migration 0018 first or rebuild"
  exit 1
fi

if ! "$ACC_BIN" tasks --help 2>&1 | grep -qi "task" && \
   ! "$ACC_BIN" 2>&1 | grep -qi "tasks"; then
  m_warn "acc-agent at ${ACC_BIN} does not support 'tasks' subcommand — rebuild first"
  exit 1
fi
m_success "acc-agent supports tasks subcommand"

# ── Linux: systemd ────────────────────────────────────────────────────────────
if on_platform linux; then
  TMPL="${WORKSPACE}/deploy/systemd/acc-task-worker.service"
  if [[ ! -f "$TMPL" ]]; then
    m_warn "systemd template not found at ${TMPL} — pull latest workspace first"
    exit 1
  fi
  systemd_install "deploy/systemd/acc-task-worker.service" "acc-task-worker.service"
  m_success "acc-task-worker.service installed and started"
fi

# ── macOS: launchd ────────────────────────────────────────────────────────────
if on_platform macos; then
  TMPL="${WORKSPACE}/deploy/launchd/com.acc.task-worker.plist"
  if [[ ! -f "$TMPL" ]]; then
    m_warn "launchd plist template not found at ${TMPL} — pull latest workspace first"
    exit 1
  fi

  PLIST_DST="${HOME}/Library/LaunchAgents/com.acc.task-worker.plist"
  mkdir -p "${HOME}/Library/LaunchAgents"
  sed "s|AGENT_USER|${USER}|g; s|AGENT_HOME|${HOME}|g" "$TMPL" > "$PLIST_DST"
  launchctl unload "$PLIST_DST" 2>/dev/null || true
  launchctl load -w "$PLIST_DST"
  m_success "com.acc.task-worker.plist loaded"
fi

m_success "Migration 0021 complete — acc-task-worker running"
m_info "Monitor with: journalctl -u acc-task-worker -f  (Linux)"
m_info "             tail -f ${ACC_DEST}/logs/task-worker.log  (macOS)"
