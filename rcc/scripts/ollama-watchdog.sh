#!/usr/bin/env bash
# ollama-watchdog.sh — health watchdog for ollama models on sparky
#
# Checks qwen2.5-coder:32b (coding fallback) every 15 min:
#   1. POST a small test prompt, expect a valid response within 30s
#   2. If degraded (timeout or garbled), restart: ollama stop + ollama run
#   3. Surface ollama_status in CCC heartbeat payload
#
# Deploy: run from cron every 15 min, or as a systemd timer.
# See: wq-NAT-idea-20260401-01

set -uo pipefail

OLLAMA_URL="${OLLAMA_URL:-http://localhost:11434}"
WATCH_MODEL="${OLLAMA_WATCH_MODEL:-qwen2.5-coder:32b}"
TIMEOUT_SECS="${OLLAMA_HEALTH_TIMEOUT:-30}"
CCC_URL="${CCC_URL:-http://146.190.134.110:8789}"
RCC_TOKEN="${RCC_TOKEN:-wq-5dcad756f6d3e345c00b5cb3dfcbdedb}"
AGENT_NAME="${AGENT_NAME:-natasha}"
LOG_FILE="${HOME}/.rcc/logs/ollama-watchdog.log"
STATUS_FILE="${HOME}/.rcc/ollama-status.json"

mkdir -p "$(dirname "$LOG_FILE")"

TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
STATUS="ok"
DETAIL=""

log() {
  echo "${TS} $*" | tee -a "$LOG_FILE"
}

# ── health check ──────────────────────────────────────────────────────────────
START_NS=$(date +%s%N 2>/dev/null || echo 0)

RESPONSE=$(curl -sf --max-time "$TIMEOUT_SECS" \
  -X POST "${OLLAMA_URL}/api/generate" \
  -H "Content-Type: application/json" \
  -d "{\"model\":\"${WATCH_MODEL}\",\"prompt\":\"1+1=\",\"stream\":false,\"options\":{\"num_predict\":4}}" \
  2>/dev/null) || RESPONSE=""

END_NS=$(date +%s%N 2>/dev/null || echo 0)
ELAPSED_MS=$(( (END_NS - START_NS) / 1000000 ))

if [[ -z "$RESPONSE" ]]; then
  STATUS="degraded"
  DETAIL="no response after ${TIMEOUT_SECS}s"
  log "WARN model=${WATCH_MODEL} status=degraded detail='${DETAIL}'"
else
  # Validate: response must contain at least one digit in output
  OUTPUT=$(echo "$RESPONSE" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('response',''))" 2>/dev/null || echo "")
  if echo "$OUTPUT" | grep -qE '[0-9]'; then
    STATUS="ok"
    log "OK model=${WATCH_MODEL} response_ms=${ELAPSED_MS} output=$(echo "$OUTPUT" | head -c 20 | tr '\n' ' ')"
  else
    STATUS="degraded"
    DETAIL="garbled output: $(echo "$OUTPUT" | head -c 80)"
    log "WARN model=${WATCH_MODEL} status=degraded detail='${DETAIL}'"
  fi
fi

# ── restart if degraded ───────────────────────────────────────────────────────
if [[ "$STATUS" == "degraded" ]]; then
  log "INFO attempting restart of ${WATCH_MODEL}"
  STATUS="restarting"

  # Stop and restart
  ollama stop "$WATCH_MODEL" 2>/dev/null || true
  sleep 3

  # Warm up with a quick generate (background, 60s max)
  timeout 60 ollama run "$WATCH_MODEL" "echo ok" &>/dev/null &
  WARM_PID=$!

  # Wait up to 60s for it to load
  WAIT=0
  while kill -0 $WARM_PID 2>/dev/null && [[ $WAIT -lt 60 ]]; do
    sleep 2; WAIT=$((WAIT + 2))
  done

  # Re-check after restart
  RECHECK=$(curl -sf --max-time 20 \
    -X POST "${OLLAMA_URL}/api/generate" \
    -H "Content-Type: application/json" \
    -d "{\"model\":\"${WATCH_MODEL}\",\"prompt\":\"2+2=\",\"stream\":false,\"options\":{\"num_predict\":4}}" \
    2>/dev/null) || RECHECK=""

  if [[ -n "$RECHECK" ]]; then
    STATUS="ok"
    log "INFO restart succeeded for ${WATCH_MODEL}"
  else
    STATUS="degraded"
    log "ERROR restart failed for ${WATCH_MODEL} — model still unresponsive"
  fi
fi

# ── write status file ─────────────────────────────────────────────────────────
cat > "$STATUS_FILE" <<EOF
{
  "model": "${WATCH_MODEL}",
  "status": "${STATUS}",
  "response_ms": ${ELAPSED_MS},
  "checked_at": "${TS}",
  "detail": "${DETAIL}"
}
EOF

# ── report to CCC heartbeat ───────────────────────────────────────────────────
curl -sf -X POST "${CCC_URL}/api/heartbeat/${AGENT_NAME}" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer ${RCC_TOKEN}" \
  -d "{\"status\":\"online\",\"host\":\"sparky\",\"ts\":\"${TS}\",\"meta\":{\"ollama_status\":\"${STATUS}\",\"ollama_model\":\"${WATCH_MODEL}\",\"ollama_response_ms\":${ELAPSED_MS}}}" \
  2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print('rcc_heartbeat:', d.get('ok','?'))" 2>/dev/null || true

log "INFO status=${STATUS} written to ${STATUS_FILE}"
