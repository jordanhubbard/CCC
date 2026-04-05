#!/usr/bin/env bash
# CCC test runner
# Run from the repo root: bash rcc/tests/run-tests.sh

set -euo pipefail

PASS=0
FAIL=0
RESULTS=()

run_test() {
  local label="$1"
  local file="$2"

  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Running: $label"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

  if node --test "$file"; then
    PASS=$((PASS + 1))
    RESULTS+=("  PASS  $label")
  else
    FAIL=$((FAIL + 1))
    RESULTS+=("  FAIL  $label")
  fi
}

# Determine repo root (parent of the directory this script lives in)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$REPO_ROOT"

run_test "api.test.mjs (local server)"      "rcc/tests/api.test.mjs"
run_test "ui.test.mjs (live UI)"            "rcc/tests/ui.test.mjs"
run_test "dashboard-regressions.test.mjs"   "rcc/tests/dashboard-regressions.test.mjs"
run_test "integration.test.mjs (live e2e)" "rcc/tests/integration.test.mjs"

# GPU tests — only run when EMBED_BACKEND=local (sparky only, skips silently otherwise)
if [ "${EMBED_BACKEND:-remote}" = "local" ]; then
  run_test "gpu/memory-pressure.test.mjs"   "rcc/tests/gpu/memory-pressure.test.mjs"
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Test Summary"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
for result in "${RESULTS[@]}"; do
  echo "$result"
done
echo ""
echo "  Passed: $PASS  Failed: $FAIL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ "$FAIL" -gt 0 ]; then
  echo "  OVERALL: FAIL"
  exit 1
else
  echo "  OVERALL: PASS"
  exit 0
fi
