#!/usr/bin/env bash
# ollama-keepalive.sh — ping ollama every 4min to keep nomic-embed-text warm in GPU memory
# Deploy on sparky (Natasha) via systemd timer.
# See: wq-NAT-1774737606949

set -euo pipefail

OLLAMA_URL="${OLLAMA_URL:-http://localhost:11434}"
MODEL="${OLLAMA_KEEPALIVE_MODEL:-nomic-embed-text}"
LOG_FILE="${HOME}/.rcc/logs/ollama-keepalive.log"

mkdir -p "$(dirname "$LOG_FILE")"

# Ping the model with a trivial embed to keep it loaded
RESPONSE=$(curl -sf -X POST "${OLLAMA_URL}/api/embeddings" \
  -H "Content-Type: application/json" \
  -d "{\"model\":\"${MODEL}\",\"prompt\":\"keepalive\"}" \
  --max-time 10 2>&1) || true

TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
if echo "${RESPONSE}" | grep -q '"embedding"'; then
  echo "${TS} OK model=${MODEL}" >> "${LOG_FILE}"
else
  echo "${TS} WARN model=${MODEL} response=${RESPONSE:0:200}" >> "${LOG_FILE}"
fi
