#!/bin/bash
# upgrade-node.sh — Non-destructive CCC agent upgrade
#
# 1. Updates the git repo (stash → fetch → ff-merge → pop)
# 2. Runs pending migrations via deploy/run-migrations.sh
# 3. Refreshes ops crons
# 4. Updates ~/.ccc/agent.json with new ccc_version
# 5. Posts a heartbeat to CCC hub
#
# Usage:
#   bash deploy/upgrade-node.sh              # normal run
#   bash deploy/upgrade-node.sh --dry-run    # print actions, don't execute
#   bash deploy/upgrade-node.sh --force      # run even if agent.json is missing
#   bash deploy/upgrade-node.sh --migrations-list   # show migration status and exit
#   bash deploy/upgrade-node.sh --no-migrations     # skip migrations

set -e

DRY_RUN=false
FORCE=false
MIGRATIONS_LIST=false
NO_MIGRATIONS=false

for arg in "$@"; do
  case "$arg" in
    --dry-run)           DRY_RUN=true ;;
    --force)             FORCE=true ;;
    --migrations-list)   MIGRATIONS_LIST=true ;;
    --no-migrations)     NO_MIGRATIONS=true ;;
  esac
done

CCC_DIR="$HOME/.ccc"
WORKSPACE="${WORKSPACE:-$CCC_DIR/workspace}"
ENV_FILE="$CCC_DIR/.env"
LOG_DIR="$CCC_DIR/logs"
AGENT_JSON="$CCC_DIR/agent.json"

# ── Colors ────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info()    { echo -e "${BLUE}[upgrade]${NC} $1"; }
success() { echo -e "${GREEN}[upgrade]${NC} ✓ $1"; }
warn()    { echo -e "${YELLOW}[upgrade]${NC} ⚠ $1"; }
error()   { echo -e "${RED}[upgrade]${NC} ✗ $1"; exit 1; }

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  CCC Agent Upgrade"
[ "$DRY_RUN" = true ] && echo "  MODE: DRY RUN — no changes will be made"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

PLATFORM="unknown"
[[ "$(uname)" == "Darwin" ]] && PLATFORM="macos"
[[ "$(uname)" == "Linux" ]]  && PLATFORM="linux"
info "Platform: $PLATFORM"

# ── Load .env ─────────────────────────────────────────────────────────────
if [ -f "$ENV_FILE" ]; then
  set -a; source "$ENV_FILE"; set +a
fi
AGENT_NAME="${AGENT_NAME:-unknown}"

# ── Migrations list mode ──────────────────────────────────────────────────
if [ "$MIGRATIONS_LIST" = true ]; then
  exec bash "$WORKSPACE/deploy/run-migrations.sh" --list
fi

# ── 1. Preflight ──────────────────────────────────────────────────────────
info "Preflight checks..."

if [ "$FORCE" = false ] && [ ! -f "$AGENT_JSON" ]; then
  error "No agent.json found at $AGENT_JSON — this node was not onboarded via CCC scripts.
  Run deploy/setup-node.sh first, or pass --force to skip this check."
fi

[ -d "$WORKSPACE/.git" ] || error "Workspace not a git repo at $WORKSPACE"

# Prefer the CCC-installed binary; fall back to PATH
CCC_AGENT="${CCC_AGENT:-$CCC_DIR/bin/ccc-agent}"
if [ ! -x "$CCC_AGENT" ]; then
  CCC_AGENT="$(command -v ccc-agent 2>/dev/null || echo "")"
fi

# ── 2. Git update ─────────────────────────────────────────────────────────
info "Updating git repo..."
cd "$WORKSPACE"

GIT_BEFORE_SHA=$(git rev-parse HEAD 2>/dev/null || echo "")

if [ "$DRY_RUN" = true ]; then
  info "  [DRY-RUN] would git fetch + merge --ff-only origin/main"
else
  STASH_OUT=$(git stash push -m "upgrade-node pre-upgrade" 2>&1 || true)
  git fetch origin --quiet
  CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
  if git rev-parse --verify "origin/$CURRENT_BRANCH" --quiet >/dev/null 2>&1; then
    git merge --ff-only "origin/$CURRENT_BRANCH" --quiet || \
      warn "Fast-forward merge failed — local changes? Proceeding with current version."
  else
    warn "No remote tracking for branch $CURRENT_BRANCH — skipping merge"
  fi
  echo "$STASH_OUT" | grep -q "No local changes to save" || git stash pop 2>/dev/null || \
    warn "Stash pop failed (non-fatal)"
fi

CCC_VERSION=$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
success "Git updated — ccc_version: $CCC_VERSION"

# ── 2b. Rebuild source-built binaries if sources changed ──────────────────
info "Checking for source changes to rebuild..."
BEFORE_SHA="$GIT_BEFORE_SHA" \
AFTER_SHA="$(git rev-parse HEAD 2>/dev/null || echo "")" \
WORKSPACE="$WORKSPACE" \
ACC_DIR="$ACC_DIR" \
LOG_DIR="$LOG_DIR" \
IS_HUB="${IS_HUB:-false}" \
DRY_RUN="$DRY_RUN" \
  bash "$WORKSPACE/deploy/rebuild-changed.sh" || \
    warn "rebuild-changed.sh failed (non-fatal)"

# ── 3. Run migrations ─────────────────────────────────────────────────────
if [ "$NO_MIGRATIONS" = false ]; then
  info "Running pending migrations..."
  MIGRATE_FLAGS=""
  [ "$DRY_RUN" = true ] && MIGRATE_FLAGS="--dry-run"
  bash "$WORKSPACE/deploy/run-migrations.sh" $MIGRATE_FLAGS || {
    warn "Migrations failed — continuing with upgrade (check migration logs)"
  }
fi

# ── 4. Reinstall ops crons ────────────────────────────────────────────────
CRON_FRAGMENT="$WORKSPACE/deploy/crontab-acc.txt"
if [ -f "$CRON_FRAGMENT" ]; then
  if crontab -l 2>/dev/null | grep -q "ccc-api-watchdog.mjs"; then
    info "Ops crons already present — skipping"
  else
    EXPANDED=$(sed "s|WORKSPACE|$WORKSPACE|g; s|LOG_DIR|$LOG_DIR|g" "$CRON_FRAGMENT" | grep -v '^#' | grep -v '^$')
    if [ "$DRY_RUN" = true ]; then
      info "  [DRY-RUN] would install ops crons from $CRON_FRAGMENT"
    else
      (crontab -l 2>/dev/null; echo "$EXPANDED") | crontab -
      success "Ops crons installed"
    fi
  fi
fi

# ── 5. Update ~/.ccc/agent.json ───────────────────────────────────────────
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
if [ "$DRY_RUN" = true ]; then
  info "  [DRY-RUN] would update $AGENT_JSON: ccc_version=$CCC_VERSION"
else
  if [ -x "$CCC_AGENT" ]; then
    if [ -f "$AGENT_JSON" ]; then
      "$CCC_AGENT" agent upgrade "$AGENT_JSON" --version="$CCC_VERSION" \
        || warn "agent.json update failed (non-fatal)"
    else
      "$CCC_AGENT" agent init "$AGENT_JSON" \
        --name="${AGENT_NAME:-unknown}" \
        --host="$(hostname)" \
        --version="$CCC_VERSION" \
        --by="upgrade-node.sh (--force)" \
        || warn "Failed to create agent.json (non-fatal)"
    fi
  else
    warn "ccc-agent not found — skipping agent.json update (run migration 0011 to build it)"
  fi
  success "agent.json updated (ccc_version=$CCC_VERSION)"
fi

# ── 6. Post heartbeat ─────────────────────────────────────────────────────
if [ -n "${CCC_URL:-}" ] && [ -n "${CCC_AGENT_TOKEN:-}" ]; then
  PAYLOAD="{\"agent\":\"$AGENT_NAME\",\"host\":\"${AGENT_HOST:-$(hostname)}\",\"ts\":\"$NOW\",\"status\":\"online\",\"ccc_version\":\"$CCC_VERSION\"}"
  if [ "$DRY_RUN" = true ]; then
    info "  [DRY-RUN] would POST heartbeat with ccc_version=$CCC_VERSION"
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

# ── Beads (bd) upgrade ───────────────────────────────────────────────────
BEADS_SRC="${BEADS_SRC:-$HOME/Src/beads}"
if [ -d "$BEADS_SRC/.git" ] && command -v go &>/dev/null; then
  if [ "$DRY_RUN" = true ]; then
    info "  [DRY-RUN] would git pull + make install-force in $BEADS_SRC"
  else
    info "Upgrading beads (bd)..."
    BEADS_BEFORE=$(git -C "$BEADS_SRC" rev-parse HEAD 2>/dev/null)
    git -C "$BEADS_SRC" pull --quiet --ff-only 2>/dev/null || true
    BEADS_AFTER=$(git -C "$BEADS_SRC" rev-parse HEAD 2>/dev/null)
    if [ "$BEADS_BEFORE" != "$BEADS_AFTER" ]; then
      (cd "$BEADS_SRC" && make install-force) 2>/dev/null \
        && success "beads rebuilt and installed ($(bd --version 2>/dev/null | head -1))" \
        || warn "beads rebuild failed"
    else
      success "beads already up to date"
    fi
  fi
elif [ ! -d "$BEADS_SRC/.git" ] && command -v go &>/dev/null; then
  warn "beads source not found at $BEADS_SRC — run setup-node.sh to install"
fi

# ── Done ──────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
[ "$DRY_RUN" = true ] \
  && echo -e "${YELLOW}Dry run complete — no changes made.${NC}" \
  || echo -e "${GREEN}✓ Upgrade complete!${NC} Agent: $AGENT_NAME | Version: $CCC_VERSION"
echo ""
