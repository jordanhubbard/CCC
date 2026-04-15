#!/bin/bash
# ccc-connectivity-check.sh — Test CCC API reachability and auto-failover to Tailscale URL
# Intended for: any agent node connecting to a remote CCC hub
# Run from: agent-pull.sh (or as a standalone cron job)

set -e

ENV_FILE="${HOME}/.ccc/.env"
LOG_FILE="${HOME}/.ccc/logs/ccc-connectivity.log"

# Load env
if [ -f "$ENV_FILE" ]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
CURRENT_CCC_URL="${CCC_URL:-}"

# Fallback endpoints (in order of preference after current URL fails)
# CCC_TAILSCALE_URL: operator-set Tailscale URL for the hub (from .env)
CCC_TAILSCALE="${CCC_TAILSCALE_URL:-}"
CCC_LOCALHOST="http://localhost:8789"

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

check_ccc() {
  local url="$1"
  local result
  result=$(curl -sf --connect-timeout 5 --max-time 8 "$url/health" 2>/dev/null || echo "FAIL")
  if echo "$result" | grep -q '"ok":true'; then
    echo "OK"
  else
    echo "FAIL"
  fi
}

update_ccc_url() {
  local new_url="$1"
  if [ -f "$ENV_FILE" ]; then
    if grep -q "^CCC_URL=" "$ENV_FILE"; then
      _sed_i "s|^CCC_URL=.*|CCC_URL=${new_url}|" "$ENV_FILE"
    else
      echo "CCC_URL=${new_url}" >> "$ENV_FILE"
    fi
    log "Updated CCC_URL to: $new_url"
    # Reload env
    set -a; source "$ENV_FILE"; set +a
  fi
}

# Skip if CCC_URL points to localhost — this node IS the hub
if echo "${CURRENT_CCC_URL}" | grep -qE '^https?://localhost(:[0-9]+)?(/|$)'; then
  log "Skipping connectivity check (CCC_URL is localhost — this is the hub node)"
  exit 0
fi

# Test current CCC_URL first
if [ -n "$CURRENT_CCC_URL" ]; then
  STATUS=$(check_ccc "$CURRENT_CCC_URL")
  if [ "$STATUS" = "OK" ]; then
    log "CCC reachable at configured URL: $CURRENT_CCC_URL"
    exit 0
  else
    log "WARNING: Cannot reach CCC at $CURRENT_CCC_URL"
  fi
fi

# Try Tailscale URL if configured (best for agents on the same tailnet)
if [ -n "$CCC_TAILSCALE" ] && [ "$CURRENT_CCC_URL" != "$CCC_TAILSCALE" ]; then
  STATUS=$(check_ccc "$CCC_TAILSCALE")
  if [ "$STATUS" = "OK" ]; then
    log "Tailscale path OK: $CCC_TAILSCALE — switching"
    update_ccc_url "$CCC_TAILSCALE"
    echo "SWITCHED_TO_TAILSCALE"
    exit 0
  else
    log "Tailscale path also unreachable: $CCC_TAILSCALE"
  fi
fi

log "ERROR: Cannot reach CCC via any known endpoint. Falling back to #agent-shared."
echo "ALL_UNREACHABLE"
exit 1
