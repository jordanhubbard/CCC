#!/usr/bin/env bash
# install-queue-worker.sh — Install the ACC queue worker daemon.
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
# Requires: ~/.acc/.env with ACC_URL and ACC_AGENT_TOKEN set.
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

echo "Installing acc-queue-worker via ${OS} (home=${AGENT_HOME}, user=${AGENT_USER})"
echo "Workspace: ${WORKSPACE}"

# Verify python3 and claude are available
python3 --version || { echo "ERROR: python3 not found" >&2; exit 1; }
claude --version 2>/dev/null || echo "WARNING: 'claude' not found in PATH — queue-worker will fail at runtime"

# Detect ACC_DIR — prefer ~/.acc, fall back to ~/.ccc for pre-migration nodes
if [[ -d "${AGENT_HOME}/.acc" ]]; then
  ACC_HOME="${AGENT_HOME}/.acc"
else
  ACC_HOME="${AGENT_HOME}/.ccc"
fi

# Verify .env
ENV_FILE="${ACC_HOME}/.env"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "ERROR: ${ENV_FILE} not found — run bootstrap.sh first." >&2
  exit 1
fi
source "$ENV_FILE"
ACC_URL="${ACC_URL:-${CCC_URL:-}}"
ACC_AGENT_TOKEN="${ACC_AGENT_TOKEN:-${CCC_AGENT_TOKEN:-}}"
[[ -z "${ACC_URL:-}" ]] && { echo "ERROR: ACC_URL not set in ${ENV_FILE}" >&2; exit 1; }
[[ -z "${ACC_AGENT_TOKEN:-}" ]] && { echo "ERROR: ACC_AGENT_TOKEN not set in ${ENV_FILE}" >&2; exit 1; }

mkdir -p "${ACC_HOME}/logs"

WORKER_SCRIPT="${ACC_HOME}/workspace/deploy/queue-worker.py"
if [[ ! -f "$WORKER_SCRIPT" ]]; then
  echo "ERROR: queue-worker.py not found at ${WORKER_SCRIPT}" >&2
  exit 1
fi

_install_nvidia_proxy() {
  local os="$1"
  local proxy_script="${ACC_HOME}/workspace/deploy/nvidia-proxy.py"
  if [[ ! -f "$proxy_script" ]]; then
    echo "WARNING: nvidia-proxy.py not found at ${proxy_script} — skipping proxy install"
    return
  fi
  if [[ "$os" == "linux" ]]; then
    local tmpl="${WORKSPACE}/deploy/systemd/acc-nvidia-proxy.service"
    local dst="/etc/systemd/system/acc-nvidia-proxy.service"
    sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$tmpl" \
      | sudo tee "$dst" > /dev/null
    echo "Wrote ${dst}"
    sudo systemctl daemon-reload
    sudo systemctl enable acc-nvidia-proxy
    sudo systemctl restart acc-nvidia-proxy
  elif [[ "$os" == "macos" ]]; then
    local tmpl="${WORKSPACE}/deploy/launchd/com.acc.nvidia-proxy.plist"
    local dst="${AGENT_HOME}/Library/LaunchAgents/com.acc.nvidia-proxy.plist"
    mkdir -p "${AGENT_HOME}/Library/LaunchAgents"
    sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$tmpl" > "$dst"
    echo "Wrote ${dst}"
    launchctl unload "$dst" 2>/dev/null || true
    launchctl load -w "$dst"
  fi
}

if [[ "$OS" == "linux" ]]; then
  SVC_TEMPLATE="${WORKSPACE}/deploy/systemd/acc-queue-worker.service"
  SVC_DST="/etc/systemd/system/acc-queue-worker.service"
  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$SVC_TEMPLATE" \
    | sudo tee "$SVC_DST" > /dev/null
  echo "Wrote ${SVC_DST}"
  sudo systemctl daemon-reload
  sudo systemctl enable acc-queue-worker
  sudo systemctl restart acc-queue-worker
  systemctl status acc-queue-worker --no-pager || true
  _install_nvidia_proxy linux

elif [[ "$OS" == "macos" ]]; then
  PLIST_TEMPLATE="${WORKSPACE}/deploy/launchd/com.acc.queue-worker.plist"
  PLIST_DST="${AGENT_HOME}/Library/LaunchAgents/com.acc.queue-worker.plist"
  mkdir -p "${AGENT_HOME}/Library/LaunchAgents"
  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$PLIST_TEMPLATE" > "$PLIST_DST"
  echo "Wrote ${PLIST_DST}"
  launchctl unload "$PLIST_DST" 2>/dev/null || true
  launchctl load -w "$PLIST_DST"
  launchctl list | grep acc.queue-worker || echo "(not yet listed — may take a moment)"
  _install_nvidia_proxy macos

elif [[ "$OS" == "supervisor" ]]; then
  CONF_TEMPLATE="${WORKSPACE}/deploy/supervisor/acc-queue-worker.conf"
  CONF_DST="/etc/supervisor/conf.d/acc-queue-worker.conf"
  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$CONF_TEMPLATE" \
    | sudo tee "$CONF_DST" > /dev/null
  echo "Wrote ${CONF_DST}"
  sudo supervisorctl reread
  sudo supervisorctl update
  echo ""
  echo "Queue worker installed (autostart=false). Start with:"
  echo "  sudo supervisorctl start acc-queue-worker"
  echo ""
  echo "⚠️  Review pending queue items before starting — it will run claude autonomously."
  sudo supervisorctl status acc-queue-worker || true
fi

echo ""
echo "Done. Tail the log to verify:"
echo "  tail -f ${ACC_HOME}/logs/queue-worker.log"
