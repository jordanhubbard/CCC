#!/bin/bash
# agent-pull.sh — Pull latest code and restart services if changed
# Runs every 10 minutes via cron or launchd
# Logs to ~/.ccc/logs/pull.log

set -e

CCC_DIR="$HOME/.ccc"
WORKSPACE="$CCC_DIR/workspace"
ENV_FILE="$CCC_DIR/.env"
LOG_FILE="$CCC_DIR/logs/pull.log"
MAX_LOG_LINES=500

# Load .env if it exists
if [ -f "$ENV_FILE" ]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
CCC_URL="${CCC_URL:-}"

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

mkdir -p "$CCC_DIR/logs"
log "Pull starting"

# ── Check repo exists ─────────────────────────────────────────────────────
if [ ! -d "$WORKSPACE/.git" ]; then
  log "ERROR: Workspace not found at $WORKSPACE — run setup-node.sh first"
  exit 1
fi

cd "$WORKSPACE"

# ── Runtime symlinks (queue.json — CCC API is authoritative source) ────────
CCC_QUEUE="$HOME/.ccc/data/queue.json"
WQ_QUEUE="$WORKSPACE/workqueue/queue.json"
if [ -f "$CCC_QUEUE" ] && [ ! -L "$WQ_QUEUE" ]; then
  ln -sf "$CCC_QUEUE" "$WQ_QUEUE" 2>/dev/null || true
fi

# ── Git pull ──────────────────────────────────────────────────────────────
BEFORE=$(git rev-parse HEAD)
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
git fetch origin --quiet 2>/dev/null || { log "ERROR: git fetch failed (network?)"; exit 1; }

if git rev-parse --verify "origin/$CURRENT_BRANCH" --quiet > /dev/null 2>&1; then
  git merge --ff-only "origin/$CURRENT_BRANCH" --quiet 2>/dev/null || {
    log "WARNING: Fast-forward merge failed on branch $CURRENT_BRANCH — local changes? Skipping."
    exit 0
  }
  log "Tracking branch: $CURRENT_BRANCH"
else
  log "No remote tracking branch for $CURRENT_BRANCH — skipping pull"
  exit 0
fi
AFTER=$(git rev-parse HEAD)

CCC_VERSION=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")

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
      launchctl kickstart -k gui/$(id -u)/com.ccc.dashboard 2>/dev/null && log "dashboard LaunchAgent restarted" || true
    fi
  fi

  # Restart ccc-api if it changed
  if echo "$CHANGED" | grep -q ".ccc/api/"; then
    log "CCC API changed — restarting ccc-api.service"
    if command -v systemctl &>/dev/null && systemctl is-active --quiet ccc-api.service 2>/dev/null; then
      sudo systemctl restart ccc-api.service && log "ccc-api.service restarted" || log "WARNING: restart failed"
    fi
  fi

  # Reinstall node deps if package.json changed
  if echo "$CHANGED" | grep -q "package.json"; then
    log "package.json changed — running npm install"
    # Fix .npm ownership (can get set to root in containers)
    if [ -d "$HOME/.npm" ]; then
      chown -R "$(id -u):$(id -g)" "$HOME/.npm" 2>/dev/null || true
    fi
    # Root deps (better-sqlite3 for exec-listener, etc.)
    if echo "$CHANGED" | grep -q "^package.json$\|^package-lock.json$"; then
      cd "$WORKSPACE" && npm install --silent && log "npm install (root) done" || log "WARNING: npm install (root) failed"
    fi
    # Dashboard deps
    if echo "$CHANGED" | grep -q "^dashboard/package"; then
      cd "$WORKSPACE/dashboard" && npm install --silent && log "npm install (dashboard) done" || log "WARNING: npm install (dashboard) failed"
    fi
    cd "$WORKSPACE"
  fi
fi

# ── Sync secrets from CCC ────────────────────────────────────────────────
# Picks up any rotated secrets without requiring re-bootstrap
if [ -n "$CCC_URL" ] && [ -n "$CCC_AGENT_TOKEN" ]; then
  SECRETS_SYNC="$WORKSPACE/deploy/secrets-sync.sh"
  if [ -f "$SECRETS_SYNC" ]; then
    if bash "$SECRETS_SYNC" >> "$LOG_FILE" 2>&1; then
      log "Secrets sync complete"
      # Reload .env in case secrets changed
      if [ -f "$ENV_FILE" ]; then
        set -a
        # shellcheck source=/dev/null
        source "$ENV_FILE"
        set +a
      fi
    else
      log "WARNING: secrets-sync.sh failed (non-fatal)"
    fi
  fi
fi

# ── Post heartbeat to CCC ─────────────────────────────────────────────────
if [ -n "$CCC_URL" ] && [ -n "$CCC_AGENT_TOKEN" ]; then
  HEARTBEAT_PAYLOAD="{\"agent\":\"$AGENT_NAME\",\"host\":\"${AGENT_HOST:-$(hostname)}\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"status\":\"online\",\"pullRev\":\"$AFTER\",\"ccc_version\":\"$CCC_VERSION\"}"
  HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST "$CCC_URL/api/heartbeat/$AGENT_NAME" \
    -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$HEARTBEAT_PAYLOAD" \
    --max-time 10 2>/dev/null)
  if [ "$HTTP_STATUS" = "200" ]; then
    log "Heartbeat posted to CCC"
  else
    log "WARNING: Heartbeat POST returned HTTP $HTTP_STATUS"
  fi
  # Update ~/.ccc/agent.json with latest ccc_version (non-fatal)
  _AGENT_BIN="${CCC_AGENT:-$CCC_DIR/bin/ccc-agent}"
  [ ! -x "$_AGENT_BIN" ] && _AGENT_BIN="$(command -v ccc-agent 2>/dev/null || echo "")"
  if [ -f "$CCC_DIR/agent.json" ] && [ -x "$_AGENT_BIN" ]; then
    "$_AGENT_BIN" agent upgrade "$CCC_DIR/agent.json" --version="$CCC_VERSION" 2>/dev/null || true
  fi
fi

log "Pull complete"
