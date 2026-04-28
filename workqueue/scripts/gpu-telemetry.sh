#!/bin/bash
# gpu-telemetry.sh — emit sparky GPU + ollama health as JSON for heartbeat payload
# Usage: source gpu-telemetry.sh && echo "$GPU_TELEMETRY_JSON"

GPU_NAME=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1 | xargs)
GPU_TEMP=$(nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader 2>/dev/null | head -1 | xargs)
GPU_POWER=$(nvidia-smi --query-gpu=power.draw --format=csv,noheader 2>/dev/null | head -1 | xargs)
VRAM_USED=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits 2>/dev/null | head -1 | xargs)
VRAM_FREE=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits 2>/dev/null | head -1 | xargs)
VRAM_TOTAL=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | head -1 | xargs)

# Ollama status
OLLAMA_STATUS="unknown"
OLLAMA_MODEL=""
if curl -sf --max-time 3 http://localhost:11434/api/tags > /tmp/ollama-tags.json 2>/dev/null; then
  OLLAMA_STATUS="ok"
  OLLAMA_MODEL=$(python3 -c "import json; d=json.load(open('/tmp/ollama-tags.json')); print(','.join(m['name'] for m in d.get('models',[]))[:80])" 2>/dev/null || "")
else
  OLLAMA_STATUS="offline"
fi

GPU_TELEMETRY_JSON=$(cat << ENDJSON
{
  "gpu": {
    "name": "${GPU_NAME:-unknown}",
    "temp_c": ${GPU_TEMP:-0},
    "power_w": "${GPU_POWER:-N/A}",
    "vram_used_mib": ${VRAM_USED:-0},
    "vram_free_mib": ${VRAM_FREE:-0},
    "vram_total_mib": ${VRAM_TOTAL:-0}
  },
  "ollama": {
    "status": "${OLLAMA_STATUS}",
    "models": "${OLLAMA_MODEL}"
  }
}
ENDJSON
)
