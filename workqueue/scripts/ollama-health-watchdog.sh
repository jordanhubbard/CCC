#!/usr/bin/env bash
# ollama-health-watchdog.sh — Health check for ollama models on sparky
#
# Checks ollama (qwen2.5-coder:32b) health via a small test prompt.
# If response time >30s or output malformed, restarts the model.
# Reports status back to CCC heartbeat payload format.
#
# Usage: ollama-health-watchdog.sh [--model <model>] [--timeout <secs>]
# Exit: 0=ok, 1=degraded (restarted), 2=unreachable

set -euo pipefail

MODEL="${1:-qwen2.5-coder:32b}"
TIMEOUT="${OLLAMA_HEALTH_TIMEOUT:-30}"
OLLAMA_BASE="${OLLAMA_BASE_URL:-http://localhost:11434}"
STATUS_FILE="${OLLAMA_STATUS_FILE:-/tmp/ollama-health-status.json}"

# Test prompt — should get a deterministic short response
TEST_PROMPT="What is 2+2? Reply with only the number."

write_status() {
  local status="$1"
  local msg="$2"
  local ts
  ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  echo "{\"ollama_status\":\"${status}\",\"model\":\"${MODEL}\",\"message\":\"${msg}\",\"ts\":\"${ts}\"}" > "$STATUS_FILE"
  echo "$status: $msg"
}

# Check if ollama is running at all
if ! curl -s --max-time 5 "${OLLAMA_BASE}/api/tags" > /dev/null 2>&1; then
  write_status "unreachable" "ollama not responding at ${OLLAMA_BASE}"
  exit 2
fi

# Check if model is available
TAGS=$(curl -s --max-time 5 "${OLLAMA_BASE}/api/tags" 2>/dev/null || echo '{}')
if ! echo "$TAGS" | grep -q "$MODEL"; then
  write_status "degraded" "model ${MODEL} not loaded — pulling/loading"
  # Attempt to load the model (pull if not present)
  ollama pull "$MODEL" 2>&1 || true
fi

# Run health check with timeout
START_TS=$(date +%s)
RESPONSE=$(timeout "$TIMEOUT" curl -s --max-time "$TIMEOUT" \
  -X POST "${OLLAMA_BASE}/api/generate" \
  -H "Content-Type: application/json" \
  -d "{\"model\":\"${MODEL}\",\"prompt\":\"${TEST_PROMPT}\",\"stream\":false,\"options\":{\"num_predict\":10}}" \
  2>/dev/null || echo "")
END_TS=$(date +%s)
ELAPSED=$((END_TS - START_TS))

if [ -z "$RESPONSE" ]; then
  write_status "degraded" "no response after ${ELAPSED}s — restarting model"
  # Restart: stop and re-run
  ollama stop "$MODEL" 2>/dev/null || true
  sleep 2
  # Warm up model in background
  ollama run "$MODEL" "test" > /dev/null 2>&1 &
  write_status "restarting" "model restart initiated after ${ELAPSED}s timeout"
  exit 1
fi

# Check response is valid JSON with a 'response' field
RESP_TEXT=$(echo "$RESPONSE" | node -e "
const d = JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'));
process.stdout.write(d.response || '');
" 2>/dev/null || echo "")

if [ -z "$RESP_TEXT" ]; then
  write_status "degraded" "malformed response (${ELAPSED}s) — restarting model"
  ollama stop "$MODEL" 2>/dev/null || true
  sleep 2
  ollama run "$MODEL" "test" > /dev/null 2>&1 &
  write_status "restarting" "model restart initiated due to malformed response"
  exit 1
fi

write_status "ok" "healthy, response in ${ELAPSED}s: '${RESP_TEXT:0:20}'"
exit 0
