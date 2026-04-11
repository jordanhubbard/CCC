#!/usr/bin/env bash
# memory-git-commit.sh — auto-commit workspace memory files to git
# Provides time-travel history for MEMORY.md and daily notes.
# Run via cron or systemd timer (daily or on session end).

set -euo pipefail

WORKSPACE="${CCC_WORKSPACE:-${WORKSPACE:-${HOME}/Src/CCC}}"
cd "$WORKSPACE"

# Check we're in a git repo
if ! git rev-parse --git-dir > /dev/null 2>&1; then
  echo "[memory-commit] Not a git repo: $WORKSPACE"
  exit 1
fi

# Stage memory files
git add -f MEMORY.md memory/ 2>/dev/null || true

# Check if there's anything to commit
if git diff --cached --quiet; then
  echo "[memory-commit] No changes to memory files — nothing to commit"
  exit 0
fi

TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
COMMIT_MSG="chore(memory): auto-snapshot ${TIMESTAMP} [natasha]"

git commit -m "$COMMIT_MSG" --no-verify 2>&1
echo "[memory-commit] Committed memory snapshot: $COMMIT_MSG"

# Push if remote is reachable (best-effort)
if git remote get-url origin > /dev/null 2>&1; then
  git push origin HEAD 2>&1 && echo "[memory-commit] Pushed to origin" || echo "[memory-commit] Push failed (offline?) — committed locally"
fi
