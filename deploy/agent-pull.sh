#!/bin/bash
# agent-pull.sh — Pull latest code and restart services if changed
# Runs every 10 minutes via cron or launchd
# Logs to ~/.acc/logs/pull.log

set -e

# Prefer ~/.acc (post-migration), fall back to ~/.ccc (pre-migration)
if [[ -d "${HOME}/.acc" ]]; then
  ACC_DIR="$HOME/.acc"
else
  ACC_DIR="$HOME/.ccc"
fi

WORKSPACE="$ACC_DIR/workspace"
ENV_FILE="$ACC_DIR/.env"
LOG_FILE="$ACC_DIR/logs/pull.log"
MAX_LOG_LINES=500

# Load .env if it exists
if [ -f "$ENV_FILE" ]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
# ACC_URL preferred; fall back to CCC_URL for pre-migration nodes
ACC_URL="${ACC_URL:-${CCC_URL:-}}"
ACC_AGENT_TOKEN="${ACC_AGENT_TOKEN:-${CCC_AGENT_TOKEN:-}}"

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

mkdir -p "$ACC_DIR/logs"
log "Pull starting"

# ── Check repo exists ─────────────────────────────────────────────────────
if [ ! -d "$WORKSPACE/.git" ]; then
  log "ERROR: Workspace not found at $WORKSPACE — run setup-node.sh first"
  exit 1
fi

cd "$WORKSPACE"

# ── Runtime symlinks (queue.json — ACC API is authoritative source) ────────
ACC_QUEUE="$ACC_DIR/data/queue.json"
WQ_QUEUE="$WORKSPACE/workqueue/queue.json"
if [ -f "$ACC_QUEUE" ] && [ ! -L "$WQ_QUEUE" ]; then
  ln -sf "$ACC_QUEUE" "$WQ_QUEUE" 2>/dev/null || true
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

ACC_VERSION=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")

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
      launchctl kickstart -k gui/$(id -u)/com.acc.dashboard 2>/dev/null && log "dashboard LaunchAgent restarted" || true
    fi
  fi

  # Rebuild acc-server if its source changed and this node runs it
  if echo "$CHANGED" | grep -q "^acc-server/"; then
    if command -v systemctl &>/dev/null && systemctl is-active --quiet acc-server.service 2>/dev/null; then
      log "acc-server source changed — rebuilding..."
      export PATH="${HOME}/.cargo/bin:${PATH}"
      if command -v cargo &>/dev/null; then
        if cargo build --release --manifest-path "${WORKSPACE}/acc-server/Cargo.toml" >> "$LOG_FILE" 2>&1; then
          BUILT="${WORKSPACE}/acc-server/target/release/acc-server"
          if sudo install -m 755 "$BUILT" /usr/local/bin/acc-server; then
            sudo systemctl restart acc-server.service && log "acc-server rebuilt and restarted" \
              || log "WARNING: acc-server restart failed"
          else
            log "WARNING: acc-server install (sudo) failed"
          fi
        else
          log "WARNING: acc-server cargo build failed"
        fi
      else
        log "WARNING: cargo not found — cannot rebuild acc-server"
      fi
    fi
  fi

  # Rebuild acc-agent if its source changed and this node runs it
  if echo "$CHANGED" | grep -q "^agent/"; then
    if command -v systemctl &>/dev/null && (systemctl is-active --quiet acc-bus-listener.service 2>/dev/null || systemctl is-active --quiet acc-queue-worker.service 2>/dev/null); then
      log "acc-agent source changed — rebuilding from local disk..."
      export PATH="${HOME}/.cargo/bin:${PATH}"
      if command -v cargo &>/dev/null; then
        # Copy to local disk to avoid FUSE mount I/O issues
        ACC_BUILD_DIR="/tmp/acc-agent-src"
        rsync -a --exclude='target/' "${WORKSPACE}/agent/" "${ACC_BUILD_DIR}/" 2>/dev/null || \
          cp -r "${WORKSPACE}/agent/." "${ACC_BUILD_DIR}/"
        if CARGO_BUILD_JOBS=1 cargo build --release --manifest-path "${ACC_BUILD_DIR}/Cargo.toml" >> "$LOG_FILE" 2>&1; then
          # Binary may be at acc-agent/target or top-level target
          BUILT="${ACC_BUILD_DIR}/target/release/acc-agent"
          [ ! -f "$BUILT" ] && BUILT="${ACC_BUILD_DIR}/acc-agent/target/release/acc-agent"
          if [ -f "$BUILT" ]; then
            INSTALL_DIR="${HOME}/.acc/bin"
            mkdir -p "$INSTALL_DIR"
            if install -m 755 "$BUILT" "${INSTALL_DIR}/acc-agent"; then
              sudo systemctl restart acc-bus-listener.service acc-queue-worker.service acc-nvidia-proxy.service 2>/dev/null || true
              log "acc-agent rebuilt and services restarted"
            else
              log "WARNING: acc-agent install failed"
            fi
          else
            log "WARNING: acc-agent binary not found after build"
          fi
        else
          log "WARNING: acc-agent cargo build failed"
        fi
      else
        log "WARNING: cargo not found — cannot rebuild acc-agent"
      fi
    fi
  fi

  # Sync Hermes skills if the vendored skills changed
  if echo "$CHANGED" | grep -q "^skills/"; then
    if [[ -d "$HOME/.hermes/skills" ]]; then
      _skill_count=0
      for _skill_dir in "$WORKSPACE/skills/agent-skills"/*/; do
        [[ -d "$_skill_dir" ]] || continue
        _skill_name="$(basename "$_skill_dir")"
        cp -r "$_skill_dir" "$HOME/.hermes/skills/${_skill_name}/" && _skill_count=$((_skill_count + 1))
      done
      for _skill_file in "$WORKSPACE/skills/superpowers"/*.md; do
        [[ -f "$_skill_file" ]] || continue
        _skill_name="$(basename "$_skill_file" .md)"
        [[ "$_skill_name" == "using-superpowers" ]] && continue
        cp "$_skill_file" "$HOME/.hermes/skills/${_skill_name}.md" && _skill_count=$((_skill_count + 1))
      done
      for _skill_dir in "$WORKSPACE/skills/superpowers"/*/; do
        [[ -d "$_skill_dir" ]] || continue
        _skill_name="$(basename "$_skill_dir")"
        cp -r "$_skill_dir" "$HOME/.hermes/skills/${_skill_name}/" && _skill_count=$((_skill_count + 1))
      done
      log "Hermes skills updated: ${_skill_count} skills synced from workspace"
    fi
  fi

  # Reinstall node deps if package.json changed
  if echo "$CHANGED" | grep -q "package.json"; then
    log "package.json changed — running npm install"
    # Fix .npm ownership (can get set to root in containers)
    if [ -d "$HOME/.npm" ]; then
      chown -R "$(id -u):$(id -g)" "$HOME/.npm" 2>/dev/null || true
    fi
    # Root deps
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

# ── Sync secrets from ACC ─────────────────────────────────────────────────
# Picks up any rotated secrets without requiring re-bootstrap
if [ -n "$ACC_URL" ] && [ -n "$ACC_AGENT_TOKEN" ]; then
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

# ── Post heartbeat to ACC ─────────────────────────────────────────────────
if [ -n "$ACC_URL" ] && [ -n "$ACC_AGENT_TOKEN" ]; then
  HEARTBEAT_PAYLOAD="{\"agent\":\"$AGENT_NAME\",\"host\":\"${AGENT_HOST:-$(hostname)}\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"status\":\"online\",\"pullRev\":\"$AFTER\",\"acc_version\":\"$ACC_VERSION\",\"ccc_version\":\"$ACC_VERSION\"}"
  HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST "$ACC_URL/api/heartbeat/$AGENT_NAME" \
    -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$HEARTBEAT_PAYLOAD" \
    --max-time 10 2>/dev/null)
  if [ "$HTTP_STATUS" = "200" ]; then
    log "Heartbeat posted to ACC"
  else
    log "WARNING: Heartbeat POST returned HTTP $HTTP_STATUS"
  fi
fi

log "Pull complete"
