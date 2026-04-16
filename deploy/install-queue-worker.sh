#!/usr/bin/env bash
# install-queue-worker.sh — Install the CCC queue worker daemon.
#
# The queue worker polls /api/queue and autonomously executes pending items
# assigned to this agent via `claude -p`. It posts keepalives and results.
#
# Run on each agent node after bootstrapping:
#
#   bash deploy/install-queue-worker.sh           # auto-detect OS
#   bash deploy/install-queue-worker.sh linux     # force Linux/systemd
#   bash deploy/install-queue-worker.sh macos     # force macOS/launchd
#   bash deploy/install-queue-worker.sh supervisor # supervisord (containers)
#
# Requires: ~/.ccc/.env with CCC_URL and CCC_AGENT_TOKEN set.
# Requires: `claude` CLI in PATH.

set -euo pipefail

AGENT_HOME="${HOME}"
AGENT_USER="${USER}"
WORKSPACE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Auto-detect service manager
OS="${1:-}"
if [[ -z "$OS" ]]; then
  if [[ "$(uname -s)" == "Darwin" ]]; then
    OS="macos"
  elif systemctl --version &>/dev/null 2>&1; then
    OS="linux"
  elif supervisorctl --version &>/dev/null 2>&1; then
    OS="supervisor"
  else
    echo "ERROR: Cannot detect service manager. Pass 'linux', 'macos', or 'supervisor'." >&2
    exit 1
  fi
fi

echo "Installing ccc-queue-worker via ${OS} (home=${AGENT_HOME}, user=${AGENT_USER})"
echo "Workspace: ${WORKSPACE}"

# Verify python3 and claude are available
python3 --version || { echo "ERROR: python3 not found" >&2; exit 1; }
claude --version 2>/dev/null || echo "WARNING: 'claude' not found in PATH — queue-worker will fail at runtime"

# Verify .env
ENV_FILE="${AGENT_HOME}/.ccc/.env"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "ERROR: ${ENV_FILE} not found — run bootstrap.sh first." >&2
  exit 1
fi
source "$ENV_FILE"
[[ -z "${CCC_URL:-}" ]] && { echo "ERROR: CCC_URL not set in ${ENV_FILE}" >&2; exit 1; }
[[ -z "${CCC_AGENT_TOKEN:-}" ]] && { echo "ERROR: CCC_AGENT_TOKEN not set in ${ENV_FILE}" >&2; exit 1; }

mkdir -p "${AGENT_HOME}/.ccc/logs"

WORKER_SCRIPT="${AGENT_HOME}/.ccc/workspace/deploy/queue-worker.py"
if [[ ! -f "$WORKER_SCRIPT" ]]; then
  echo "ERROR: queue-worker.py not found at ${WORKER_SCRIPT}" >&2
  exit 1
fi

if [[ "$OS" == "linux" ]]; then
  SVC_TEMPLATE="${WORKSPACE}/deploy/systemd/ccc-queue-worker.service"
  SVC_DST="/etc/systemd/system/ccc-queue-worker.service"
  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$SVC_TEMPLATE" \
    | sudo tee "$SVC_DST" > /dev/null
  echo "Wrote ${SVC_DST}"
  sudo systemctl daemon-reload
  sudo systemctl enable ccc-queue-worker
  sudo systemctl restart ccc-queue-worker
  systemctl status ccc-queue-worker --no-pager || true

elif [[ "$OS" == "macos" ]]; then
  PLIST_TEMPLATE="${WORKSPACE}/deploy/launchd/com.ccc.queue-worker.plist"
  PLIST_DST="${AGENT_HOME}/Library/LaunchAgents/com.ccc.queue-worker.plist"
  mkdir -p "${AGENT_HOME}/Library/LaunchAgents"
  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$PLIST_TEMPLATE" > "$PLIST_DST"
  echo "Wrote ${PLIST_DST}"
  launchctl unload "$PLIST_DST" 2>/dev/null || true
  launchctl load -w "$PLIST_DST"
  launchctl list | grep ccc.queue-worker || echo "(not yet listed — may take a moment)"

elif [[ "$OS" == "supervisor" ]]; then
  CONF_TEMPLATE="${WORKSPACE}/deploy/supervisor/ccc-queue-worker.conf"
  CONF_DST="/etc/supervisor/conf.d/ccc-queue-worker.conf"
  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$CONF_TEMPLATE" \
    | sudo tee "$CONF_DST" > /dev/null
  echo "Wrote ${CONF_DST}"
  sudo supervisorctl reread
  sudo supervisorctl update
  # Note: autostart=false — start manually when ready
  echo ""
  echo "Queue worker installed (autostart=false). Start with:"
  echo "  sudo supervisorctl start ccc-queue-worker"
  echo ""
  echo "⚠️  Review pending queue items before starting — it will run claude autonomously."
  sudo supervisorctl status ccc-queue-worker || true
fi

echo ""
echo "Done. Tail the log to verify:"
echo "  tail -f ${AGENT_HOME}/.ccc/logs/queue-worker.log"
