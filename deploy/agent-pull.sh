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

mkdir -p "$RCC_DIR/logs"
log "Pull starting"

# ── Check repo exists ──────────────────────────────────────────────────────
if [ ! -d "$WORKSPACE/.git" ]; then
  log "ERROR: Workspace not found at $WORKSPACE — run setup-node.sh first"
  exit 1
fi

cd "$WORKSPACE"

# ── Runtime symlinks (queue.json — CCC API is authoritative source) ────────
RCC_QUEUE="$HOME/.rcc/data/queue.json"
WQ_QUEUE="$WORKSPACE/workqueue/queue.json"
if [ -f "$RCC_QUEUE" ] && [ ! -L "$WQ_QUEUE" ]; then
  ln -sf "$RCC_QUEUE" "$WQ_QUEUE" 2>/dev/null || true
fi

# ── Git pull ───────────────────────────────────────────────────────────────
BEFORE=$(git rev-parse HEAD)
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
git fetch origin --quiet 2>/dev/null || { log "ERROR: git fetch failed (network?)"; exit 1; }

# Check if remote tracking branch exists for current branch
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
    log "CCC API changed — restarting ccc-api.service"
    if command -v systemctl &>/dev/null && systemctl is-active --quiet ccc-api.service 2>/dev/null; then
      sudo systemctl restart ccc-api.service && log "ccc-api.service restarted" || log "WARNING: restart failed"
    fi
  fi

  # Reinstall node deps if package.json changed
  if echo "$CHANGED" | grep -q "package.json"; then
    log "package.json changed — running npm install"
    cd "$WORKSPACE/dashboard" && npm install --silent && cd "$WORKSPACE"
    log "npm install done"
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

# ── Sync secrets from CCC ─────────────────────────────────────────────────
# Refreshes ~/.rcc/.env with latest secret values from the CCC secrets store.
# Never clobbers CCC_AGENT_TOKEN (identity key) or AGENT_NAME/AGENT_HOST.
# Runs on every pull to pick up rotated credentials automatically.
if [ -n "$CCC_URL" ] && [ -n "$CCC_AGENT_TOKEN" ] && command -v node >/dev/null 2>&1; then
  _env_file="$HOME/.rcc/.env"
  _sync_count=0
  # macOS vs Linux sed -i compatibility
  if [ "$(uname)" = "Darwin" ]; then
    _sed_i() { sed -i '' "$@"; }
  else
    _sed_i() { sed -i "$@"; }
  fi
  _set_env_key() {
    local _key="$1" _val="$2"
    case "$_key" in CCC_AGENT_TOKEN|CCC_URL|AGENT_NAME|AGENT_HOST) return ;; esac
    if grep -q "^${_key}=" "$_env_file" 2>/dev/null; then
      _sed_i "s|^${_key}=.*|${_key}=${_val}|" "$_env_file"
    else
      echo "${_key}=${_val}" >> "$_env_file"
    fi
    _sync_count=$((_sync_count + 1))
  }
  for _alias in slack mattermost minio milvus nvidia github; do
    _resp=$(curl -sf --max-time 5 \
      -H "Authorization: Bearer ${CCC_AGENT_TOKEN}" \
      "${CCC_URL}/api/secrets/${_alias}" 2>/dev/null || true)
    [ -z "$_resp" ] && continue
    if echo "$_resp" | grep -q '"secrets"'; then
      while IFS='=' read -r _k _v; do
        [ -z "$_k" ] && continue
        _set_env_key "$_k" "$_v"
      done < <(node -e "
        try {
          const d = JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'));
          const s = d.secrets || {};
          for (const [k,v] of Object.entries(s)) console.log(k+'='+v);
        } catch(e) {}
      " <<< "$_resp" 2>/dev/null)
    fi
  done
  if [ "$_sync_count" -gt 0 ]; then
    chmod 600 "$_env_file"
    log "Secrets synced from CCC: ${_sync_count} var(s) updated"
    # Reload env after sync
    set -a; source "$_env_file"; set +a
  fi
fi

# ── Post heartbeat to CCC ─────────────────────────────────────────────────
if [ -n "$CCC_URL" ] && [ -n "$CCC_AGENT_TOKEN" ]; then
  HEARTBEAT_PAYLOAD="{\"agent\":\"$AGENT_NAME\",\"host\":\"${AGENT_HOST:-$(hostname)}\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"status\":\"online\",\"pullRev\":\"$AFTER\"}"
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
fi

# ── Sync shared openclaw identity files ───────────────────────────────────
OPENCLAW_WORKSPACE="$HOME/.openclaw/workspace"
SHARED_DIR="$WORKSPACE/openclaw/shared"
if [ -d "$OPENCLAW_WORKSPACE" ] && [ -d "$SHARED_DIR" ]; then
  for f in "$SHARED_DIR"/*.md; do
    [ -f "$f" ] || continue
    fname=$(basename "$f")
    # Never overwrite soul/memory/heartbeat/tools — those are local
    case "$fname" in
      MEMORY.md|HEARTBEAT.md|TOOLS.md|SOUL.md|IDENTITY.md) continue ;;
    esac
    cp "$f" "$OPENCLAW_WORKSPACE/$fname"
  done
  log "Synced shared openclaw files"
fi

log "Pull complete"
