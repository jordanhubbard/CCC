#!/usr/bin/env bash
# install-bus-listener.sh — Install and enable the AgentBus SSE listener daemon.
#
# Run this on each agent node after bootstrapping:
#
#   bash deploy/install-bus-listener.sh          # auto-detect OS
#   bash deploy/install-bus-listener.sh linux    # force Linux/systemd
#   bash deploy/install-bus-listener.sh macos    # force macOS/launchd
#
# Requires: ~/.acc/.env with ACC_URL and ACC_AGENT_TOKEN set.

set -euo pipefail

AGENT_HOME="${HOME}"
AGENT_USER="${USER}"
WORKSPACE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Auto-detect OS
OS="${1:-}"
if [[ -z "$OS" ]]; then
  case "$(uname -s)" in
    Darwin) OS="macos" ;;
    Linux)  OS="linux" ;;
    *)      echo "ERROR: Unknown OS $(uname -s). Pass 'linux' or 'macos' explicitly." >&2; exit 1 ;;
  esac
fi

echo "Installing acc-bus-listener on ${OS} (home=${AGENT_HOME}, user=${AGENT_USER})"
echo "Workspace: ${WORKSPACE}"

# Detect ACC_DIR — prefer ~/.acc, fall back to ~/.ccc for pre-migration nodes
if [[ -d "${AGENT_HOME}/.acc" ]]; then
  ACC_HOME="${AGENT_HOME}/.acc"
else
  ACC_HOME="${AGENT_HOME}/.ccc"
fi

# Verify .env exists and has required vars
ENV_FILE="${ACC_HOME}/.env"
if [[ ! -f "$ENV_FILE" ]]; then
  echo "ERROR: ${ENV_FILE} not found — run bootstrap.sh first." >&2
  exit 1
fi
source "$ENV_FILE"
ACC_URL="${ACC_URL:-${CCC_URL:-}}"
ACC_AGENT_TOKEN="${ACC_AGENT_TOKEN:-${CCC_AGENT_TOKEN:-}}"
if [[ -z "${ACC_URL:-}" ]]; then
  echo "ERROR: ACC_URL not set in ${ENV_FILE}" >&2
  exit 1
fi
if [[ -z "${ACC_AGENT_TOKEN:-}" ]]; then
  echo "WARNING: ACC_AGENT_TOKEN not set — bus-listener will connect but hub may reject without auth"
fi

mkdir -p "${ACC_HOME}/logs"

if [[ "$OS" == "linux" ]]; then
  SVC_TEMPLATE="${WORKSPACE}/deploy/systemd/acc-bus-listener.service"
  SVC_DST="/etc/systemd/system/acc-bus-listener.service"

  if [[ ! -f "$SVC_TEMPLATE" ]]; then
    echo "ERROR: service template not found at ${SVC_TEMPLATE}" >&2
    exit 1
  fi

  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$SVC_TEMPLATE" \
    | sudo tee "$SVC_DST" > /dev/null
  echo "Wrote ${SVC_DST}"

  sudo systemctl daemon-reload
  sudo systemctl enable acc-bus-listener
  sudo systemctl restart acc-bus-listener
  echo ""
  systemctl status acc-bus-listener --no-pager || true

elif [[ "$OS" == "macos" ]]; then
  PLIST_TEMPLATE="${WORKSPACE}/deploy/launchd/com.acc.bus-listener.plist"
  PLIST_DST="${AGENT_HOME}/Library/LaunchAgents/com.acc.bus-listener.plist"

  if [[ ! -f "$PLIST_TEMPLATE" ]]; then
    echo "ERROR: plist template not found at ${PLIST_TEMPLATE}" >&2
    exit 1
  fi

  mkdir -p "${AGENT_HOME}/Library/LaunchAgents"
  sed "s|AGENT_USER|${AGENT_USER}|g; s|AGENT_HOME|${AGENT_HOME}|g" "$PLIST_TEMPLATE" > "$PLIST_DST"
  echo "Wrote ${PLIST_DST}"

  # Unload if already running
  launchctl unload "$PLIST_DST" 2>/dev/null || true
  launchctl load -w "$PLIST_DST"
  echo ""
  launchctl list | grep acc.bus-listener || echo "(not yet listed — may take a moment)"
fi

echo ""
echo "Done. Tail the log to verify:"
echo "  tail -f ${ACC_HOME}/logs/bus-listener.log"
