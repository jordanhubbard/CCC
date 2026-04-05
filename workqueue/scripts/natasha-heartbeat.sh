#!/usr/bin/env bash
# natasha-heartbeat.sh — Enhanced heartbeat for Natasha on sparky
#
# Posts heartbeat to CCC with GPU telemetry (vRAM, temp, power) and
# ollama model status. Called by cron or workqueue processor.
#
# Usage: natasha-heartbeat.sh
# Exit: 0=ok, 1=partial (sent without GPU data), 2=failed

set -euo pipefail

CCC_URL="${CCC_URL:-https://rcc.yourmom.photos}"
RCC_TOKEN="${RCC_TOKEN:-wq-07ebee759ffbbf31b2d265651a117f16661d2e13}"
AGENT_NAME="natasha"
HOST="sparky"

TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# --- GPU telemetry via nvidia-smi ---
GPU_NAME=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1 | xargs || echo "unknown")
GPU_TEMP=$(nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader 2>/dev/null | head -1 | xargs || echo "0")
GPU_POWER=$(nvidia-smi --query-gpu=power.draw --format=csv,noheader,nounits 2>/dev/null | head -1 | xargs || echo "0")
VRAM_USED=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits 2>/dev/null | head -1 | xargs || echo "0")
VRAM_FREE=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits 2>/dev/null | head -1 | xargs || echo "0")
VRAM_TOTAL=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | head -1 | xargs || echo "0")

# Handle [N/A] values (GB10 shared memory reports N/A)
[[ "$VRAM_USED" == *"N/A"* ]] && VRAM_USED="null"
[[ "$VRAM_FREE" == *"N/A"* ]] && VRAM_FREE="null"
[[ "$VRAM_TOTAL" == *"N/A"* ]] && VRAM_TOTAL="null"
[[ "$GPU_POWER" == *"N/A"* ]] && GPU_POWER="null"
[[ "$GPU_TEMP" == *"N/A"* ]] && GPU_TEMP="null"

# Numeric guard: if still not null/numeric, use null
is_number() { [[ "$1" =~ ^[0-9]+(\.[0-9]+)?$ ]]; }
is_number "$GPU_TEMP" || GPU_TEMP="null"
is_number "$GPU_POWER" || GPU_POWER="null"
is_number "$VRAM_USED" || VRAM_USED="null"
is_number "$VRAM_FREE" || VRAM_FREE="null"
is_number "$VRAM_TOTAL" || VRAM_TOTAL="null"

# --- Ollama status ---
OLLAMA_STATUS="offline"
OLLAMA_MODELS="[]"
if curl -sf --max-time 3 http://localhost:11434/api/tags > /tmp/ollama-tags-hb.json 2>/dev/null; then
  OLLAMA_STATUS="ok"
  OLLAMA_MODELS=$(python3 -c "
import json
d = json.load(open('/tmp/ollama-tags-hb.json'))
models = [m['name'] for m in d.get('models', [])]
print(json.dumps(models))
" 2>/dev/null || echo "[]")
fi

# Check cached ollama_status from watchdog if available
WATCHDOG_STATUS=""
if [ -f /tmp/ollama-health-status.json ]; then
  WATCHDOG_STATUS=$(python3 -c "
import json
d = json.load(open('/tmp/ollama-health-status.json'))
print(d.get('ollama_status', ''))
" 2>/dev/null || echo "")
fi
[ -n "$WATCHDOG_STATUS" ] && OLLAMA_STATUS="$WATCHDOG_STATUS"

# --- Build payload ---
PAYLOAD=$(cat << ENDJSON
{
  "status": "online",
  "host": "${HOST}",
  "ts": "${TS}",
  "gpu": {
    "name": "${GPU_NAME}",
    "temp_c": ${GPU_TEMP},
    "power_w": ${GPU_POWER},
    "vram_used_mib": ${VRAM_USED},
    "vram_free_mib": ${VRAM_FREE},
    "vram_total_mib": ${VRAM_TOTAL}
  },
  "ollama": {
    "status": "${OLLAMA_STATUS}",
    "models": ${OLLAMA_MODELS}
  }
}
ENDJSON
)

# --- POST heartbeat ---
HTTP_CODE=$(curl -s -o /tmp/natasha-hb-response.json -w "%{http_code}" \
  -X POST "${CCC_URL}/api/heartbeat/${AGENT_NAME}" \
  -H "Authorization: Bearer ${RCC_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" \
  --max-time 10 2>/dev/null)

if [ "$HTTP_CODE" -ge 200 ] && [ "$HTTP_CODE" -lt 300 ]; then
  echo "Heartbeat OK (${HTTP_CODE}) — GPU: ${GPU_NAME}, ${GPU_TEMP}°C, ${GPU_POWER}W — ollama: ${OLLAMA_STATUS}"
  exit 0
else
  echo "Heartbeat FAILED (${HTTP_CODE})" >&2
  cat /tmp/natasha-hb-response.json >&2
  exit 2
fi
