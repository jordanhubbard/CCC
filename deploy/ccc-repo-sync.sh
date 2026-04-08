#!/bin/bash
# ccc-repo-sync.sh — Git pull + auto-commit + push for the ClawFS shared CCC repo
#
# This runs on exactly ONE node (the designated CCC_REPO_PUSHER, typically Rocky).
# It keeps the shared ClawFS repo in sync with GitHub:
#   1. Pull latest from origin (ff-only)
#   2. Auto-commit any local changes (from agents editing files on ClawFS)
#   3. Push to origin
#
# Usage:
#   bash deploy/ccc-repo-sync.sh           # one-shot
#   CCC_REPO_SYNC_DRY_RUN=1 bash deploy/ccc-repo-sync.sh  # dry run
#
# Designed to run via systemd timer or cron every 30 minutes.

set -euo pipefail

CCC_DIR="$HOME/.ccc"
ENV_FILE="$CCC_DIR/.env"
LOG_FILE="$CCC_DIR/logs/repo-sync.log"
MAX_LOG_LINES=500

# Load .env if it exists
if [ -f "$ENV_FILE" ]; then
  set -a
  source "$ENV_FILE"
  set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
DRY_RUN="${CCC_REPO_SYNC_DRY_RUN:-0}"

# Resolve the repo path
CLAWFS_CCC_REPO="${CLAWFS_CCC_REPO:-$HOME/clawfs/repos/CCC}"
WORKSPACE="$CCC_DIR/workspace"

# Use ClawFS repo if available, fall back to workspace
if [ -d "$CLAWFS_CCC_REPO/.git" ]; then
  REPO="$CLAWFS_CCC_REPO"
elif [ -d "$WORKSPACE/.git" ]; then
  REPO="$WORKSPACE"
else
  echo "ERROR: No repo found at $CLAWFS_CCC_REPO or $WORKSPACE" >&2
  exit 1
fi

mkdir -p "$(dirname "$LOG_FILE")"

log() {
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] [$AGENT_NAME] [repo-sync] $1" >> "$LOG_FILE" 2>&1
}

# Rotate log
if [ -f "$LOG_FILE" ]; then
  lines=$(wc -l < "$LOG_FILE")
  if [ "$lines" -gt "$MAX_LOG_LINES" ]; then
    tail -n "$MAX_LOG_LINES" "$LOG_FILE" > "${LOG_FILE}.tmp" && mv "${LOG_FILE}.tmp" "$LOG_FILE"
  fi
fi

log "Sync starting (repo: $REPO, dry_run: $DRY_RUN)"

cd "$REPO"

# ── Step 1: Pull latest from origin ──────────────────────────────────────────
BEFORE=$(git rev-parse HEAD)
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)

git fetch origin --quiet 2>/dev/null || {
  log "ERROR: git fetch failed (network?)"
  exit 1
}

if git rev-parse --verify "origin/$CURRENT_BRANCH" --quiet > /dev/null 2>&1; then
  # Stash any local changes before pull to avoid merge conflicts
  STASH_NEEDED=false
  if ! git diff --quiet 2>/dev/null || ! git diff --cached --quiet 2>/dev/null; then
    STASH_NEEDED=true
    git stash --quiet 2>/dev/null || true
    log "Stashed local changes before pull"
  fi

  git merge --ff-only "origin/$CURRENT_BRANCH" --quiet 2>/dev/null || {
    log "WARNING: Fast-forward merge failed — diverged from origin. Skipping pull."
    if [ "$STASH_NEEDED" = true ]; then
      git stash pop --quiet 2>/dev/null || true
    fi
  }

  if [ "$STASH_NEEDED" = true ]; then
    git stash pop --quiet 2>/dev/null || {
      log "WARNING: stash pop had conflicts — check manually"
    }
  fi
fi

AFTER_PULL=$(git rev-parse HEAD)
if [ "$BEFORE" != "$AFTER_PULL" ]; then
  log "Pulled: $BEFORE -> $AFTER_PULL"
else
  log "Already up to date with origin"
fi

# ── Step 2: Auto-commit local changes ────────────────────────────────────────
# Ignore common runtime/temp files
git add -A 2>/dev/null

# Check if there's anything to commit
if git diff --cached --quiet 2>/dev/null; then
  log "No local changes to commit"
else
  CHANGED_FILES=$(git diff --cached --name-only | head -20 | tr '\n' ' ')
  COMMIT_MSG="auto-sync $(date -u +%Y%m%dT%H%M%SZ) [$AGENT_NAME]: $CHANGED_FILES"

  if [ "$DRY_RUN" = "1" ]; then
    log "DRY RUN: Would commit: $COMMIT_MSG"
    git reset HEAD --quiet 2>/dev/null || true
  else
    git commit -m "$COMMIT_MSG" --quiet 2>/dev/null
    log "Committed: $COMMIT_MSG"
  fi
fi

# ── Step 3: Push to origin ───────────────────────────────────────────────────
if [ "$DRY_RUN" = "1" ]; then
  log "DRY RUN: Would push to origin/$CURRENT_BRANCH"
else
  # Only push if we have commits ahead of origin
  AHEAD=$(git rev-list "origin/$CURRENT_BRANCH..HEAD" --count 2>/dev/null || echo "0")
  if [ "$AHEAD" -gt 0 ]; then
    git push origin "$CURRENT_BRANCH" --quiet 2>/dev/null || {
      log "WARNING: git push failed — will retry next cycle"
    }
    log "Pushed $AHEAD commit(s) to origin/$CURRENT_BRANCH"
  else
    log "Nothing to push"
  fi
fi

log "Sync complete"
