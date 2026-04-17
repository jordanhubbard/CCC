#!/bin/bash
# run-migrations.sh — Django-style incremental migrations for ACC agent nodes
#
# Scans deploy/migrations/NNNN_*.sh in sorted order, runs each one that hasn't
# been applied yet on this node, and records the result in ~/.acc/migrations.json.
#
# Each migration script is sourced with these variables pre-set:
#   WORKSPACE, ACC_DIR, LOG_DIR, AGENT_NAME, PLATFORM, DRY_RUN
# and these helper functions:
#   m_info, m_success, m_warn, m_skip — colored output
#   on_platform PLATFORM   — returns 0 if current platform matches
#   service_exists NAME    — returns 0 if systemd/launchd unit is installed
#
# Usage:
#   bash deploy/run-migrations.sh [--dry-run] [--list] [--from=NNNN] [--only=NNNN]
#
# Options:
#   --dry-run      Print what would run, don't execute or record anything
#   --list         List migrations and their applied status, then exit
#   --from=NNNN    Only run migrations with number >= NNNN
#   --only=NNNN    Only run the migration with exactly this number
#   --force=NNNN   Re-run migration NNNN even if already applied
#   --reset        Clear all migration records (use with care)

set -euo pipefail

# ── Parse args ────────────────────────────────────────────────────────────
DRY_RUN=false
LIST_ONLY=false
FROM_NUM=0
ONLY_NUM=""
FORCE_NUM=""
RESET=false

for arg in "$@"; do
  case "$arg" in
    --dry-run)    DRY_RUN=true ;;
    --list)       LIST_ONLY=true ;;
    --from=*)     FROM_NUM="${arg#*=}" ;;
    --only=*)     ONLY_NUM="${arg#*=}" ;;
    --force=*)    FORCE_NUM="${arg#*=}" ;;
    --reset)      RESET=true ;;
    *) echo "Unknown argument: $arg" >&2; exit 1 ;;
  esac
done

# ── Environment ───────────────────────────────────────────────────────────
# Prefer ~/.acc (post-migration), fall back to ~/.ccc (pre-migration)
if [[ -d "${HOME}/.acc" ]]; then
  ACC_DIR="${ACC_DIR:-$HOME/.acc}"
else
  ACC_DIR="${ACC_DIR:-${CCC_DIR:-$HOME/.ccc}}"
fi
# Export as CCC_DIR too for backward compat with older migration scripts
CCC_DIR="$ACC_DIR"
WORKSPACE="${WORKSPACE:-$ACC_DIR/workspace}"
LOG_DIR="${LOG_DIR:-$ACC_DIR/logs}"
MIGRATIONS_DIR="$WORKSPACE/deploy/migrations"
MIGRATIONS_JSON="$ACC_DIR/migrations.json"

if [ -f "$ACC_DIR/.env" ]; then
  set -a; source "$ACC_DIR/.env"; set +a
fi
AGENT_NAME="${AGENT_NAME:-unknown}"

PLATFORM="unknown"
[[ "$(uname)" == "Darwin" ]] && PLATFORM="macos"
[[ "$(uname)" == "Linux" ]]  && PLATFORM="linux"

# ── Colors ────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; CYAN='\033[0;36m'; NC='\033[0m'

m_info()    { echo -e "${BLUE}  [migration]${NC} $1"; }
m_success() { echo -e "${GREEN}  [migration]${NC} ✓ $1"; }
m_warn()    { echo -e "${YELLOW}  [migration]${NC} ⚠ $1"; }
m_skip()    { echo -e "${CYAN}  [migration]${NC} → $1"; }

# ── Helper functions exported to migration scripts ────────────────────────
on_platform() { [ "$PLATFORM" = "$1" ]; }

service_exists() {
  local name="$1"
  if [ "$PLATFORM" = "linux" ] && command -v systemctl >/dev/null 2>&1; then
    systemctl list-unit-files "$name" 2>/dev/null | grep -q "$name" || \
    systemctl status "$name" 2>/dev/null | grep -q "Loaded:"
  elif [ "$PLATFORM" = "macos" ]; then
    launchctl list 2>/dev/null | grep -q "$name"
  else
    return 1
  fi
}

systemd_teardown() {
  # systemd_teardown NAME PATH1 [PATH2 ...]
  local name="$1"; shift
  if [ "$PLATFORM" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then return 0; fi
  if systemctl list-unit-files "$name" 2>/dev/null | grep -q "$name" || \
     [ -f "/etc/systemd/system/$name" ] || [ -f "/usr/lib/systemd/system/$name" ]; then
    sudo systemctl stop "$name" 2>/dev/null || true
    sudo systemctl disable "$name" 2>/dev/null || true
    m_success "stopped+disabled: $name"
  fi
  for path in "$@"; do
    local p="${path/\~/$HOME}"
    if [ -f "$p" ]; then
      sudo rm -f "$p"
      m_success "removed unit: $p"
    fi
  done
  sudo systemctl daemon-reload 2>/dev/null || true
  sudo systemctl reset-failed 2>/dev/null || true
}

systemd_install() {
  # systemd_install SOURCE_REL_PATH NAME
  local src="$WORKSPACE/$1" name="$2"
  if [ "$PLATFORM" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then return 0; fi
  if [ ! -f "$src" ]; then m_warn "source not found: $src"; return 1; fi
  sed "s|AGENT_USER|$(whoami)|g; s|AGENT_HOME|$HOME|g" "$src" | sudo tee "/etc/systemd/system/$name" > /dev/null
  sudo systemctl daemon-reload
  sudo systemctl enable "$name" 2>/dev/null || true
  sudo systemctl restart "$name" 2>/dev/null || sudo systemctl start "$name" 2>/dev/null || \
    m_warn "failed to start $name (non-fatal)"
  m_success "installed+started: $name"
}

launchd_teardown() {
  # launchd_teardown LABEL PATH1 [PATH2 ...]
  local label="$1"; shift
  if [ "$PLATFORM" != "macos" ]; then return 0; fi
  for path in "$@"; do
    local p="${path/\~/$HOME}"
    if [ -f "$p" ]; then
      launchctl unload "$p" 2>/dev/null || true
      rm -f "$p"
      m_success "unloaded+removed: $p ($label)"
    fi
  done
}

launchd_install() {
  # launchd_install SOURCE_REL_PATH INSTALL_PATH LABEL
  local src="$WORKSPACE/$1" dst="${2/\~/$HOME}" label="$3"
  if [ "$PLATFORM" != "macos" ]; then return 0; fi
  if [ ! -f "$src" ]; then m_warn "source not found: $src"; return 1; fi
  if launchctl list 2>/dev/null | grep -q "$label"; then
    launchctl unload "$dst" 2>/dev/null || true
  fi
  local pull_script="$WORKSPACE/deploy/agent-pull.sh"
  sed "s|PULL_SCRIPT_PATH|$pull_script|g; s|LOG_PATH|$LOG_DIR/pull.log|g" "$src" > "$dst"
  launchctl load "$dst"
  m_success "installed+loaded: $label"
}

cron_remove() {
  # cron_remove PATTERN [PATTERN ...]
  for pattern in "$@"; do
    if crontab -l 2>/dev/null | grep -q "$pattern"; then
      (crontab -l 2>/dev/null | grep -v "$pattern") | crontab - || true
      m_success "removed cron matching: $pattern"
    fi
  done
}

export -f on_platform service_exists systemd_teardown systemd_install launchd_teardown launchd_install cron_remove m_info m_success m_warn m_skip

export ACC_DIR CCC_DIR WORKSPACE LOG_DIR AGENT_NAME PLATFORM DRY_RUN RED GREEN YELLOW BLUE CYAN NC

# ── Migration state backend: python3 fallback ─────────────────────────────
CCC_AGENT="${CCC_AGENT:-$ACC_DIR/bin/ccc-agent}"
if [ ! -x "$CCC_AGENT" ]; then
  CCC_AGENT="$(command -v ccc-agent 2>/dev/null || echo "")"
fi

is_applied() {
  local name="$1"
  if [ -x "$CCC_AGENT" ]; then
    "$CCC_AGENT" migrate is-applied "$name"
  else
    python3 - "$MIGRATIONS_JSON" "$name" << 'PYEOF' 2>/dev/null
import json, sys
path, name = sys.argv[1], sys.argv[2]
try:
    d = json.load(open(path))
    sys.exit(0 if d.get(name, {}).get('status') == 'ok' else 1)
except Exception:
    sys.exit(1)
PYEOF
  fi
}

record_applied() {
  local name="$1" status="$2"
  if [ -x "$CCC_AGENT" ]; then
    "$CCC_AGENT" migrate record "$name" "$status"
  else
    python3 - "$MIGRATIONS_JSON" "$name" "$status" << 'PYEOF' 2>/dev/null
import json, sys, os
from datetime import datetime, timezone
path, name, status = sys.argv[1], sys.argv[2], sys.argv[3]
os.makedirs(os.path.dirname(path), exist_ok=True)
try:
    d = json.load(open(path)) if os.path.exists(path) else {}
except Exception:
    d = {}
d[name] = {'status': status, 'appliedAt': datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}
with open(path, 'w') as f:
    json.dump(d, f, indent=2)
    f.write('\n')
PYEOF
  fi
}

# ── Load/initialise migrations.json ──────────────────────────────────────
if [ "$RESET" = true ]; then
  echo "{}" > "$MIGRATIONS_JSON"
  echo "Migration records reset."
  exit 0
fi

mkdir -p "$(dirname "$MIGRATIONS_JSON")"
if [ ! -f "$MIGRATIONS_JSON" ]; then
  echo "{}" > "$MIGRATIONS_JSON"
fi

# ── Discover migrations ───────────────────────────────────────────────────
if [ ! -d "$MIGRATIONS_DIR" ]; then
  echo "No migrations directory at $MIGRATIONS_DIR"
  exit 0
fi

# mapfile is bash 4+ only; use a while-read loop for macOS bash 3.2 compat
migration_files=()
while IFS= read -r f; do
  migration_files+=("$f")
done < <(ls "$MIGRATIONS_DIR"/[0-9][0-9][0-9][0-9]_*.sh 2>/dev/null | sort)

if [ ${#migration_files[@]} -eq 0 ]; then
  echo "No migration files found in $MIGRATIONS_DIR"
  exit 0
fi

# ── List mode ─────────────────────────────────────────────────────────────
if [ "$LIST_ONLY" = true ]; then
  echo ""
  echo "ACC Migrations — $AGENT_NAME ($PLATFORM)"
  echo "───────────────────────────────────────────────"
  if [ -x "$CCC_AGENT" ]; then
    "$CCC_AGENT" migrate list "$MIGRATIONS_DIR"
  else
    # python3 fallback for list mode (used before migration 0011 installs ccc-agent)
    for f in "${migration_files[@]}"; do
      name=$(basename "$f" .sh)
      num="${name%%_*}"
      desc=$(grep -m1 '^# Description:' "$f" 2>/dev/null | sed 's/# Description: //' || echo "$name")
      if is_applied "$name" 2>/dev/null; then
        echo -e "  ${GREEN}✓${NC} [$num] $desc"
      else
        echo -e "  ${YELLOW}○${NC} [$num] $desc  (pending)"
      fi
    done
  fi
  echo ""
  exit 0
fi

# ── Run migrations ────────────────────────────────────────────────────────
pending=0
applied=0
failed=0

for f in "${migration_files[@]}"; do
  name=$(basename "$f" .sh)
  num="${name%%_*}"
  desc=$(grep -m1 '^# Description:' "$f" 2>/dev/null | sed 's/# Description: //' || echo "$name")

  # Filtering
  if [ -n "$ONLY_NUM" ] && [ "$num" != "$ONLY_NUM" ]; then continue; fi
  if [ -n "$FROM_NUM" ] && [ "$num" -lt "$FROM_NUM" ] 2>/dev/null; then continue; fi

  # Skip if already applied (unless force)
  if [ "$FORCE_NUM" != "$num" ] && is_applied "$name" 2>/dev/null; then
    m_skip "[$num] $desc (already applied)"
    continue
  fi

  pending=$((pending + 1))
  echo ""
  echo -e "${BLUE}━━ Migration $num: $desc${NC}"

  if [ "$DRY_RUN" = true ]; then
    echo -e "${YELLOW}  [DRY-RUN]${NC} would run: $f"
    continue
  fi

  # Run the migration in a subshell so it can't accidentally exit the runner
  if (
    set -euo pipefail
    # shellcheck source=/dev/null
    source "$f"
  ); then
    record_applied "$name" "ok"
    m_success "[$num] $desc — done"
    applied=$((applied + 1))
  else
    EXIT_CODE=$?
    record_applied "$name" "failed"
    m_warn "[$num] $desc — FAILED (exit $EXIT_CODE)"
    failed=$((failed + 1))
    # Stop on first failure (like Django)
    break
  fi
done

echo ""
echo "───────────────────────────────────────────────"
if [ "$DRY_RUN" = true ]; then
  echo "Dry run complete. $pending migration(s) would run."
elif [ "$failed" -gt 0 ]; then
  echo -e "${RED}Migrations halted: $applied applied, $failed failed.${NC}"
  exit 1
else
  echo -e "${GREEN}Migrations complete: $applied applied, $failed failed.${NC}"
fi
echo ""
