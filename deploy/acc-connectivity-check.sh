#!/bin/bash
# acc-connectivity-check.sh — Test ACC API reachability and auto-failover to Tailscale URL
# Intended for: any agent node connecting to a remote ACC hub
# Run from: agent-pull.sh (or as a standalone cron job)

set -e

ENV_FILE="${HOME}/.acc/.env"
LOG_FILE="${HOME}/.acc/logs/acc-connectivity.log"

# Load env
if [ -f "$ENV_FILE" ]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
CURRENT_ACC_URL="${ACC_URL:-}"

# Fallback endpoints (in order of preference after current URL fails)
# ACC_TAILSCALE_URL: operator-set Tailscale URL for the hub (from .env)
ACC_TAILSCALE="${ACC_TAILSCALE_URL:-}"
ACC_LOCALHOST="http://localhost:8789"

log() {
  mkdir -p "$(dirname "$LOG_FILE")"
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] [$AGENT_NAME] $1" >> "$LOG_FILE" 2>&1
}

# macOS vs Linux sed -i
if [ "$(uname)" = "Darwin" ]; then
  _sed_i() { sed -i '' "$@"; }
else
  _sed_i() { sed -i "$@"; }
fi

check_acc() {
  local url="$1"
  local result
  result=$(curl -sf --connect-timeout 5 --max-time 8 "$url/health" 2>/dev/null || echo "FAIL")
  if echo "$result" | grep -q '"ok":true'; then
    echo "OK"
  else
    echo "FAIL"
  fi
}

update_acc_url() {
  local new_url="$1"
  if [ -f "$ENV_FILE" ]; then
    if grep -q "^ACC_URL=" "$ENV_FILE"; then
      _sed_i "s|^ACC_URL=.*|ACC_URL=${new_url}|" "$ENV_FILE"
    else
      echo "ACC_URL=${new_url}" >> "$ENV_FILE"
    fi
    log "Updated ACC_URL to: $new_url"
    # Reload env
    set -a; source "$ENV_FILE"; set +a
  fi
}

# Skip if ACC_URL points to localhost — this node IS the hub
if echo "${CURRENT_ACC_URL}" | grep -qE '^https?://localhost(:[0-9]+)?(/|$)'; then
  log "Skipping connectivity check (ACC_URL is localhost — this is the hub node)"
  exit 0
fi

# Test current ACC_URL first
if [ -n "$CURRENT_ACC_URL" ]; then
  STATUS=$(check_acc "$CURRENT_ACC_URL")
  if [ "$STATUS" = "OK" ]; then
    log "ACC reachable at configured URL: $CURRENT_ACC_URL"
    exit 0
  else
    log "WARNING: Cannot reach ACC at $CURRENT_ACC_URL"
  fi
fi

# Try Tailscale URL if configured (best for agents on the same tailnet)
if [ -n "$ACC_TAILSCALE" ] && [ "$CURRENT_ACC_URL" != "$ACC_TAILSCALE" ]; then
  STATUS=$(check_acc "$ACC_TAILSCALE")
  if [ "$STATUS" = "OK" ]; then
    log "Tailscale path OK: $ACC_TAILSCALE — switching"
    update_acc_url "$ACC_TAILSCALE"
    echo "SWITCHED_TO_TAILSCALE"
    exit 0
  else
    log "Tailscale path also unreachable: $ACC_TAILSCALE"
  fi
fi

log "ERROR: Cannot reach ACC via any known endpoint. Falling back to #agent-shared."
echo "ALL_UNREACHABLE"
exit 1
