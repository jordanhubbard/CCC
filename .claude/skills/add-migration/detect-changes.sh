#!/bin/bash
# detect-changes.sh — Identify deploy-relevant changes that need a migration
# Run from the repo root. Outputs a structured report to stdout.

MIGRATIONS_DIR="deploy/migrations"
LAST_MIGRATION=$(ls "$MIGRATIONS_DIR"/[0-9][0-9][0-9][0-9]_*.sh 2>/dev/null | sort | tail -1)
LAST_NUM=0
if [ -n "$LAST_MIGRATION" ]; then
  LAST_NUM=$(basename "$LAST_MIGRATION" | cut -d_ -f1 | sed 's/^0*//')
fi
NEXT_NUM=$(printf "%04d" $((LAST_NUM + 1)))

echo "=== NEXT MIGRATION NUMBER: $NEXT_NUM ==="
echo ""

echo "=== EXISTING MIGRATIONS ==="
ls "$MIGRATIONS_DIR"/[0-9][0-9][0-9][0-9]_*.sh 2>/dev/null | sort | while read -r f; do
  num=$(basename "$f" | cut -d_ -f1)
  desc=$(grep -m1 '^# Description:' "$f" 2>/dev/null | sed 's/# Description: //' || basename "$f" .sh)
  echo "  [$num] $desc"
done
echo ""

echo "=== DEPLOY-RELEVANT CHANGES (git diff HEAD) ==="
git diff HEAD --name-only 2>/dev/null | grep -E \
  '^deploy/(systemd|launchd|migrations|crontab|ccc-agent|setup-node|bootstrap|agent-pull|upgrade-node|run-migrations)' \
  || echo "  (none detected in staged/unstaged diff)"
echo ""

echo "=== DEPLOY-RELEVANT STAGED CHANGES ==="
git diff --cached --name-only 2>/dev/null | grep -E \
  '^deploy/(systemd|launchd|migrations|crontab|ccc-agent|setup-node|bootstrap|agent-pull|upgrade-node|run-migrations)' \
  || echo "  (none)"
echo ""

echo "=== RECENT COMMITS (for context) ==="
git log --oneline -5 2>/dev/null
echo ""

echo "=== CURRENT SERVICE FILES ==="
echo "-- systemd --"
ls deploy/systemd/*.service deploy/systemd/*.timer 2>/dev/null | xargs -I{} basename {} | sort
echo "-- launchd --"
ls deploy/launchd/*.plist 2>/dev/null | xargs -I{} basename {} | sort
echo ""

echo "=== HELPER FUNCTIONS AVAILABLE IN MIGRATIONS ==="
echo "  on_platform linux|macos"
echo "  systemd_install  RELATIVE_SOURCE_PATH  UNIT_NAME"
echo "  systemd_teardown UNIT_NAME  UNIT_PATH..."
echo "  launchd_install  RELATIVE_SOURCE_PATH  INSTALL_PATH  LABEL"
echo "  launchd_teardown LABEL  PLIST_PATH..."
echo "  cron_remove      GREP_PATTERN..."
echo "  m_info / m_success / m_warn / m_skip"
