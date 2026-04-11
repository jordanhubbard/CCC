#!/bin/bash
# upgrade-node.sh — Non-destructive CCC agent upgrade
#
# Tears down blacklisted (legacy) services, installs/restarts whitelisted ones,
# updates git, and writes the latest ccc_version to ~/.ccc/agent.json.
#
# Usage:
#   bash deploy/upgrade-node.sh              # normal run
#   bash deploy/upgrade-node.sh --dry-run    # print actions, don't execute
#   bash deploy/upgrade-node.sh --force      # run even if agent.json is missing
#
# Must be run from inside the CCC workspace (or with WORKSPACE set).

set -e

DRY_RUN=false
FORCE=false
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=true ;;
    --force)   FORCE=true ;;
  esac
done

CCC_DIR="$HOME/.ccc"
WORKSPACE="${WORKSPACE:-$CCC_DIR/workspace}"
ENV_FILE="$CCC_DIR/.env"
LOG_DIR="$CCC_DIR/logs"
AGENT_JSON="$CCC_DIR/agent.json"
BLACKLIST="$WORKSPACE/deploy/services-blacklist.json"
WHITELIST="$WORKSPACE/deploy/services-whitelist.json"

# ── Colors ────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info()    { echo -e "${BLUE}[upgrade]${NC} $1"; }
success() { echo -e "${GREEN}[upgrade]${NC} ✓ $1"; }
warn()    { echo -e "${YELLOW}[upgrade]${NC} ⚠ $1"; }
error()   { echo -e "${RED}[upgrade]${NC} ✗ $1"; exit 1; }
dry()     { echo -e "${YELLOW}[DRY-RUN]${NC} would: $1"; }

run() {
  # run CMD [description]
  local cmd="$1"
  local desc="${2:-$1}"
  if [ "$DRY_RUN" = true ]; then
    dry "$desc"
  else
    eval "$cmd" || warn "Command failed (non-fatal): $desc"
  fi
}

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  CCC Agent Upgrade"
[ "$DRY_RUN" = true ] && echo "  MODE: DRY RUN — no changes will be made"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── Detect platform ────────────────────────────────────────────────────────
PLATFORM="unknown"
[[ "$(uname)" == "Darwin" ]] && PLATFORM="macos"
[[ "$(uname)" == "Linux" ]]  && PLATFORM="linux"
info "Platform: $PLATFORM"

# ── 1. Preflight ──────────────────────────────────────────────────────────
info "Preflight checks..."

if [ "$FORCE" = false ] && [ ! -f "$AGENT_JSON" ]; then
  error "No agent.json found at $AGENT_JSON — this node was not onboarded via CCC scripts.
  Run deploy/setup-node.sh first, or pass --force to skip this check."
fi

if [ ! -d "$WORKSPACE/.git" ]; then
  error "Workspace not a git repo at $WORKSPACE"
fi

if [ ! -f "$BLACKLIST" ]; then
  error "services-blacklist.json not found at $BLACKLIST"
fi

if [ ! -f "$WHITELIST" ]; then
  error "services-whitelist.json not found at $WHITELIST"
fi

if ! command -v node >/dev/null 2>&1; then
  error "node is required but not found"
fi

# ── Load .env ─────────────────────────────────────────────────────────────
if [ -f "$ENV_FILE" ]; then
  set -a; source "$ENV_FILE"; set +a
fi
AGENT_NAME="${AGENT_NAME:-unknown}"

# ── 2. Git update ─────────────────────────────────────────────────────────
info "Updating git repo..."
cd "$WORKSPACE"
if [ "$DRY_RUN" = true ]; then
  dry "git stash push -m 'upgrade-node pre-upgrade'"
  dry "git fetch origin"
  dry "git merge --ff-only origin/main"
  dry "git stash pop"
else
  STASH_OUT=$(git stash push -m "upgrade-node pre-upgrade" 2>&1 || true)
  git fetch origin --quiet
  CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
  if git rev-parse --verify "origin/$CURRENT_BRANCH" --quiet >/dev/null 2>&1; then
    git merge --ff-only "origin/$CURRENT_BRANCH" --quiet || warn "Fast-forward merge failed — local changes? Proceeding with current version."
  else
    warn "No remote tracking for branch $CURRENT_BRANCH — skipping merge"
  fi
  echo "$STASH_OUT" | grep -q "No local changes to save" || git stash pop 2>/dev/null || warn "Stash pop failed (non-fatal)"
fi
CCC_VERSION=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
success "Git updated — ccc_version: $CCC_VERSION"

# ── 3. Blacklist teardown ─────────────────────────────────────────────────
info "Tearing down blacklisted services..."

_blacklist_entries() {
  # Emit one line per entry: TYPE|NAME|PATHS_SEMICOLON|CRON_PATTERNS_SEMICOLON|REASON
  node -e "
    const bl = JSON.parse(require('fs').readFileSync('$BLACKLIST', 'utf8'));
    for (const e of bl) {
      const paths = (e.unit_paths || []).join(';');
      const patterns = (e.cron_patterns || []).join(';');
      console.log([e.type||'', e.name||'', paths, patterns, (e.reason||'').replace(/\n/g,' ')].join('|'));
    }
  "
}

while IFS='|' read -r svc_type svc_name unit_paths_raw cron_patterns_raw reason; do
  [ -z "$svc_type" ] && continue
  info "  Blacklist: [$svc_type] $svc_name — $reason"

  case "$svc_type" in
    systemd)
      if [ "$PLATFORM" = "linux" ] && command -v systemctl >/dev/null 2>&1; then
        # Check if the unit exists before trying to stop/disable
        if systemctl list-unit-files "$svc_name" 2>/dev/null | grep -q "$svc_name" || \
           systemctl status "$svc_name" 2>/dev/null | grep -q "Loaded:"; then
          run "sudo systemctl stop '$svc_name' 2>/dev/null || true" "systemctl stop $svc_name"
          run "sudo systemctl disable '$svc_name' 2>/dev/null || true" "systemctl disable $svc_name"
          success "  Stopped+disabled: $svc_name"
        else
          info "  Not active/installed: $svc_name (skipping)"
        fi
        # Remove unit files
        IFS=';' read -ra paths_arr <<< "$unit_paths_raw"
        for p in "${paths_arr[@]}"; do
          [ -z "$p" ] && continue
          p_expanded="${p/\~/$HOME}"
          if [ -f "$p_expanded" ]; then
            run "sudo rm -f '$p_expanded'" "rm $p_expanded"
            success "  Removed unit file: $p_expanded"
          fi
        done
        run "sudo systemctl daemon-reload 2>/dev/null || true" "systemctl daemon-reload"
        run "sudo systemctl reset-failed 2>/dev/null || true" "systemctl reset-failed"
      fi
      ;;
    launchd)
      if [ "$PLATFORM" = "macos" ]; then
        IFS=';' read -ra paths_arr <<< "$unit_paths_raw"
        for p in "${paths_arr[@]}"; do
          [ -z "$p" ] && continue
          p_expanded="${p/\~/$HOME}"
          if [ -f "$p_expanded" ]; then
            run "launchctl unload '$p_expanded' 2>/dev/null || true" "launchctl unload $p_expanded"
            run "rm -f '$p_expanded'" "rm $p_expanded"
            success "  Unloaded+removed: $p_expanded"
          fi
        done
      fi
      ;;
    cron)
      if [ -n "$cron_patterns_raw" ]; then
        IFS=';' read -ra patterns_arr <<< "$cron_patterns_raw"
        for pattern in "${patterns_arr[@]}"; do
          [ -z "$pattern" ] && continue
          if crontab -l 2>/dev/null | grep -q "$pattern"; then
            if [ "$DRY_RUN" = true ]; then
              dry "crontab -l | grep -v '$pattern' | crontab -"
            else
              (crontab -l 2>/dev/null | grep -v "$pattern") | crontab - || true
              success "  Removed cron matching: $pattern"
            fi
          else
            info "  No cron matching: $pattern"
          fi
        done
      fi
      ;;
  esac
done < <(_blacklist_entries)

# ── 4. Competing process teardown ─────────────────────────────────────────
info "Stopping competing processes (openclaw gateway, claw-watchdog)..."
# Graceful SIGTERM — do NOT delete ~/.openclaw data
run "pkill -TERM -f 'openclaw.*gateway' 2>/dev/null || true" "pkill openclaw gateway"
run "pkill -TERM -f 'claw-watchdog' 2>/dev/null || true" "pkill claw-watchdog"
info "  (Data in ~/.openclaw/ is preserved)"

# ── 5. Whitelist install ──────────────────────────────────────────────────
info "Installing/refreshing whitelisted services..."
PULL_SCRIPT="$WORKSPACE/deploy/agent-pull.sh"

_whitelist_entries() {
  node -e "
    const wl = JSON.parse(require('fs').readFileSync('$WHITELIST', 'utf8'));
    for (const e of wl) {
      console.log([e.type||'', e.name||'', e.condition||'', e.source||'', e.install_path||'', e.label||''].join('|'));
    }
  "
}

# Determine active conditions for this node
CONDITIONS=()
[ "$PLATFORM" = "linux" ] && CONDITIONS+=(linux)
[ "$PLATFORM" = "macos" ] && CONDITIONS+=(macos)
# Hub: if ccc-server is in the whitelist source at this path
if [ -f "$WORKSPACE/ccc/dashboard/ccc-server/src/main.rs" ]; then
  CONDITIONS+=(hub)
fi
# GPU: nvidia-smi present
command -v nvidia-smi >/dev/null 2>&1 && CONDITIONS+=(gpu)
# macos_claude: only if tmux + claude available on macOS
[ "$PLATFORM" = "macos" ] && command -v tmux >/dev/null 2>&1 && command -v claude >/dev/null 2>&1 && CONDITIONS+=(macos_claude)
# sparky/bridge/minio/whisper/ci: check env vars (operator sets these)
[ "${IS_SPARKY:-false}" = "true" ] && CONDITIONS+=(sparky)
[ "${IS_BRIDGE:-false}" = "true" ] && CONDITIONS+=(bridge)
[ "${IS_MINIO:-false}" = "true" ] && CONDITIONS+=(minio)
[ "${IS_WHISPER:-false}" = "true" ] && CONDITIONS+=(whisper)
[ "${IS_CI:-false}" = "true" ] && CONDITIONS+=(ci)

has_condition() {
  local needle="$1"
  for c in "${CONDITIONS[@]}"; do [ "$c" = "$needle" ] && return 0; done
  return 1
}

while IFS='|' read -r svc_type svc_name condition source_rel install_path label; do
  [ -z "$svc_type" ] && continue
  has_condition "$condition" || { info "  Skip (condition not met): $svc_name [$condition]"; continue; }

  info "  Install: [$svc_type] $svc_name"
  source_path="$WORKSPACE/$source_rel"

  case "$svc_type" in
    systemd)
      if [ "$PLATFORM" = "linux" ] && command -v systemctl >/dev/null 2>&1; then
        if [ -f "$source_path" ]; then
          run "sudo cp '$source_path' '/etc/systemd/system/$svc_name'" "cp $svc_name to /etc/systemd/system/"
          run "sudo systemctl daemon-reload" "systemctl daemon-reload"
          run "sudo systemctl enable '$svc_name'" "systemctl enable $svc_name"
          run "sudo systemctl restart '$svc_name' 2>/dev/null || sudo systemctl start '$svc_name'" "systemctl (re)start $svc_name"
          success "  Installed+started: $svc_name"
        else
          warn "  Source not found: $source_path"
        fi
      fi
      ;;
    launchd)
      if [ "$PLATFORM" = "macos" ] && [ -n "$install_path" ]; then
        install_path_expanded="${install_path/\~/$HOME}"
        if [ -f "$source_path" ]; then
          if [ -n "$label" ] && launchctl list 2>/dev/null | grep -q "$label"; then
            run "launchctl unload '$install_path_expanded' 2>/dev/null || true" "launchctl unload $label"
          fi
          run "sed \"s|PULL_SCRIPT_PATH|$PULL_SCRIPT|g; s|LOG_PATH|$LOG_DIR/pull.log|g\" '$source_path' > '$install_path_expanded'" \
              "install $svc_name to $install_path_expanded"
          run "launchctl load '$install_path_expanded'" "launchctl load $svc_name"
          success "  Installed+loaded: $svc_name ($label)"
        else
          warn "  Source not found: $source_path"
        fi
      fi
      ;;
  esac
done < <(_whitelist_entries)

# ── 6. Reinstall ops crons ────────────────────────────────────────────────
info "Refreshing ops crons..."
CRON_FRAGMENT="$WORKSPACE/deploy/crontab-ccc.txt"
if [ -f "$CRON_FRAGMENT" ]; then
  if crontab -l 2>/dev/null | grep -q "ccc-api-watchdog.mjs"; then
    info "  Ops crons already present — skipping"
  else
    EXPANDED=$(sed "s|WORKSPACE|$WORKSPACE|g; s|LOG_DIR|$LOG_DIR|g" "$CRON_FRAGMENT" | grep -v '^#' | grep -v '^$')
    if [ "$DRY_RUN" = true ]; then
      dry "append ops crons from $CRON_FRAGMENT to crontab"
    else
      (crontab -l 2>/dev/null; echo "$EXPANDED") | crontab -
      success "Ops crons installed"
    fi
  fi
else
  warn "Ops cron fragment not found at $CRON_FRAGMENT — skipping"
fi

# ── 7. Update ~/.ccc/agent.json ───────────────────────────────────────────
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
if [ "$DRY_RUN" = true ]; then
  dry "update $AGENT_JSON: ccc_version=$CCC_VERSION last_upgraded_at=$NOW"
else
  if [ -f "$AGENT_JSON" ]; then
    node -e "
      try {
        const f='$AGENT_JSON';
        const d=JSON.parse(require('fs').readFileSync(f,'utf8'));
        d.ccc_version='$CCC_VERSION';
        d.last_upgraded_at='$NOW';
        d.last_upgraded_version='$CCC_VERSION';
        require('fs').writeFileSync(f,JSON.stringify(d,null,2)+'\n');
      } catch(e){ process.stderr.write('agent.json update failed: '+e.message+'\n'); }
    " 2>&1 || warn "agent.json update failed (non-fatal)"
  else
    # First migration of an old agent — create from scratch
    node -e "
      require('fs').writeFileSync('$AGENT_JSON', JSON.stringify({
        schema_version: 1,
        agent_name: '${AGENT_NAME:-unknown}',
        host: '$(hostname)',
        onboarded_at: '$NOW',
        onboarded_by: 'upgrade-node.sh (--force migration)',
        ccc_version: '$CCC_VERSION',
        last_upgraded_at: '$NOW',
        last_upgraded_version: '$CCC_VERSION'
      }, null, 2) + '\n');
    " && chmod 600 "$AGENT_JSON" || warn "Failed to create agent.json (non-fatal)"
  fi
  success "agent.json updated (ccc_version=$CCC_VERSION)"
fi

# ── 8. Post heartbeat ─────────────────────────────────────────────────────
if [ -n "${CCC_URL:-}" ] && [ -n "${CCC_AGENT_TOKEN:-}" ]; then
  PAYLOAD="{\"agent\":\"$AGENT_NAME\",\"host\":\"${AGENT_HOST:-$(hostname)}\",\"ts\":\"$NOW\",\"status\":\"online\",\"ccc_version\":\"$CCC_VERSION\"}"
  if [ "$DRY_RUN" = true ]; then
    dry "POST $CCC_URL/api/heartbeat/$AGENT_NAME with ccc_version=$CCC_VERSION"
  else
    HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
      -X POST "$CCC_URL/api/heartbeat/$AGENT_NAME" \
      -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
      -H "Content-Type: application/json" \
      -d "$PAYLOAD" \
      --max-time 10 2>/dev/null)
    [ "$HTTP_STATUS" = "200" ] && success "Heartbeat posted (ccc_version=$CCC_VERSION)" \
      || warn "Heartbeat returned HTTP $HTTP_STATUS (non-fatal)"
  fi
fi

# ── Done ──────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
[ "$DRY_RUN" = true ] \
  && echo -e "${YELLOW}Dry run complete — no changes made.${NC}" \
  || echo -e "${GREEN}✓ Upgrade complete!${NC} Agent: $AGENT_NAME | Version: $CCC_VERSION"
echo ""
