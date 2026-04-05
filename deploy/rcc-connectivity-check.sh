#!/bin/bash
# rcc-connectivity-check.sh — Test CCC API reachability and auto-failover to Tailscale IP
# Intended for: sparky (Natasha) and any agent that may be behind NAT/firewall
# Run from: agent-pull.sh (or as a standalone cron job)
# Created: 2026-03-27 by Rocky (wq-API-1774652964439)

set -e

ENV_FILE="${HOME}/.rcc/.env"
LOG_FILE="${HOME}/.rcc/logs/rcc-connectivity.log"

# Load env
if [ -f "$ENV_FILE" ]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
CURRENT_RCC_URL="${CCC_URL:-}"

# Known CCC endpoints (in order of preference)
RCC_PUBLIC="http://146.190.134.110:8789"
RCC_TAILSCALE="http://100.89.199.14:8789"
RCC_LOCALHOST="http://localhost:8789"

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

check_rcc() {
  local url="$1"
  local result
  result=$(curl -sf --connect-timeout 5 --max-time 8 "$url/health" 2>/dev/null || echo "FAIL")
  if echo "$result" | grep -q '"ok":true'; then
    echo "OK"
  else
    echo "FAIL"
  fi
}

update_rcc_url() {
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

# Skip if we're running on do-host1 (CCC itself)
if [ "${AGENT_HOST}" = "do-host1" ] || [ "${AGENT_NAME}" = "rocky" ]; then
  log "Skipping connectivity check (running on CCC host)"
  exit 0
fi

# Test current CCC_URL first
if [ -n "$CURRENT_RCC_URL" ]; then
  STATUS=$(check_rcc "$CURRENT_RCC_URL")
  if [ "$STATUS" = "OK" ]; then
    log "CCC reachable at configured URL: $CURRENT_RCC_URL"
    exit 0
  else
    log "WARNING: Cannot reach CCC at $CURRENT_RCC_URL"
  fi
fi

# Try Tailscale IP (best for agents on the same tailnet)
if [ "$CURRENT_RCC_URL" != "$RCC_TAILSCALE" ]; then
  STATUS=$(check_rcc "$RCC_TAILSCALE")
  if [ "$STATUS" = "OK" ]; then
    log "Tailscale path OK: $RCC_TAILSCALE — switching"
    update_rcc_url "$RCC_TAILSCALE"
    echo "SWITCHED_TO_TAILSCALE"
    exit 0
  else
    log "Tailscale path also unreachable: $RCC_TAILSCALE"
  fi
fi

# Try public IP
if [ "$CURRENT_RCC_URL" != "$RCC_PUBLIC" ]; then
  STATUS=$(check_rcc "$RCC_PUBLIC")
  if [ "$STATUS" = "OK" ]; then
    log "Public IP path OK: $RCC_PUBLIC — switching"
    update_rcc_url "$RCC_PUBLIC"
    echo "SWITCHED_TO_PUBLIC"
    exit 0
  else
    log "Public IP also unreachable: $RCC_PUBLIC"
  fi
fi

log "ERROR: Cannot reach CCC via any known endpoint. Falling back to #agent-shared."
echo "ALL_UNREACHABLE"
exit 1
