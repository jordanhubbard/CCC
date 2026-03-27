#!/bin/bash
# agent-pull.sh — Pull latest code and restart services if changed
# Runs every 10 minutes via cron or launchd
# Logs to ~/.rcc/logs/pull.log

set -e

RCC_DIR="$HOME/.rcc"
WORKSPACE="$RCC_DIR/workspace"
ENV_FILE="$RCC_DIR/.env"
LOG_FILE="$RCC_DIR/logs/pull.log"
MAX_LOG_LINES=500

# Load .env if it exists
if [ -f "$ENV_FILE" ]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
RCC_URL="${RCC_URL:-}"

log() {
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] [$AGENT_NAME] $1" >> "$LOG_FILE" 2>&1
}

# ── Rotate log ─────────────────────────────────────────────────────────────
if [ -f "$LOG_FILE" ]; then
  lines=$(wc -l < "$LOG_FILE")
  if [ "$lines" -gt "$MAX_LOG_LINES" ]; then
    tail -n "$MAX_LOG_LINES" "$LOG_FILE" > "${LOG_FILE}.tmp" && mv "${LOG_FILE}.tmp" "$LOG_FILE"
  fi
fi

mkdir -p "$RCC_DIR/logs"
log "Pull starting"

# ── Check repo exists ──────────────────────────────────────────────────────
if [ ! -d "$WORKSPACE/.git" ]; then
  log "ERROR: Workspace not found at $WORKSPACE — run setup-node.sh first"
  exit 1
fi

cd "$WORKSPACE"

# ── Git pull ───────────────────────────────────────────────────────────────
BEFORE=$(git rev-parse HEAD)
git fetch origin --quiet 2>/dev/null || { log "ERROR: git fetch failed (network?)"; exit 1; }
git merge --ff-only origin/master --quiet 2>/dev/null || git merge --ff-only origin/main --quiet 2>/dev/null || {
  log "WARNING: Fast-forward merge failed — local changes? Skipping."
  exit 0
}
AFTER=$(git rev-parse HEAD)

if [ "$BEFORE" = "$AFTER" ]; then
  log "No changes"
else
  log "Updated: $BEFORE -> $AFTER"
  
  # Check what changed
  CHANGED=$(git diff --name-only "$BEFORE" "$AFTER")
  log "Changed files: $(echo "$CHANGED" | tr '\n' ' ')"

  # Restart dashboard if it changed
  if echo "$CHANGED" | grep -q "^dashboard/"; then
    log "Dashboard changed — restarting wq-dashboard.service"
    if command -v systemctl &>/dev/null && systemctl is-active --quiet wq-dashboard.service 2>/dev/null; then
      sudo systemctl restart wq-dashboard.service && log "wq-dashboard.service restarted" || log "WARNING: restart failed"
    elif command -v launchctl &>/dev/null; then
      launchctl kickstart -k gui/$(id -u)/com.rcc.dashboard 2>/dev/null && log "dashboard LaunchAgent restarted" || true
    fi
  fi

  # Restart rcc-api if it changed
  if echo "$CHANGED" | grep -q "^rcc/api/"; then
    log "RCC API changed — restarting rcc-api.service"
    if command -v systemctl &>/dev/null && systemctl is-active --quiet rcc-api.service 2>/dev/null; then
      sudo systemctl restart rcc-api.service && log "rcc-api.service restarted" || log "WARNING: restart failed"
    fi
  fi

  # Reinstall node deps if package.json changed
  if echo "$CHANGED" | grep -q "package.json"; then
    log "package.json changed — running npm install"
    cd "$WORKSPACE/dashboard" && npm install --silent && cd "$WORKSPACE"
    log "npm install done"
  fi
fi

# ── Post heartbeat to RCC ─────────────────────────────────────────────────
if [ -n "$RCC_URL" ] && [ -n "$RCC_AGENT_TOKEN" ]; then
  HEARTBEAT_PAYLOAD="{\"agent\":\"$AGENT_NAME\",\"host\":\"${AGENT_HOST:-$(hostname)}\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"status\":\"online\",\"pullRev\":\"$AFTER\"}"
  HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST "$RCC_URL/api/heartbeat/$AGENT_NAME" \
    -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$HEARTBEAT_PAYLOAD" \
    --max-time 10 2>/dev/null)
  if [ "$HTTP_STATUS" = "200" ]; then
    log "Heartbeat posted to RCC"
  else
    log "WARNING: Heartbeat POST returned HTTP $HTTP_STATUS"
  fi
fi

log "Pull complete"
