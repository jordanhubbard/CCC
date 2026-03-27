#!/bin/bash
# Rocky heartbeat writer + dashboard regenerator
# Runs every cron cycle (:07 and :37), writes heartbeat to MinIO and rebuilds dashboard HTML

MC=/home/jkh/.local/bin/mc
QUEUE_FILE="/home/jkh/.openclaw/workspace/workqueue/queue.json"
AZURE_SAS="https://loomdd566f62.blob.core.windows.net/assets/agent-dashboard.html?se=2029-03-19T02%3A25Z&sp=rwdlcu&spr=https&sv=2026-02-06&ss=b&srt=sco&sig=Dn4faVsJCz0ufWyHmiKCFCrgiLQkSIRtp7MLmqXKiUA%3D"

# Get queue depth
PENDING=$(node -e "const q=require('$QUEUE_FILE'); console.log(q.items.filter(i=>i.status==='pending').length)" 2>/dev/null || echo 0)

# Check services
MINIO_UP="ok"; SEARXNG_UP="ok"
curl -sf http://localhost:9000/minio/health/live > /dev/null 2>&1 || MINIO_UP="error"
curl -sf "http://localhost:8888/search?q=ping&format=json" > /dev/null 2>&1 || SEARXNG_UP="error"

TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# Write Rocky heartbeat to MinIO
cat > /tmp/rocky-heartbeat.json << JSON
{
  "agent": "rocky",
  "ts": "$TS",
  "status": "online",
  "host": "${AGENT_HOST:-localhost}",
  "queue_depth": $PENDING,
  "last_error": null,
  "services": {
    "minio": "$MINIO_UP",
    "searxng": "$SEARXNG_UP"
  }
}
JSON
$MC cp /tmp/rocky-heartbeat.json ${MINIO_ALIAS:-local}/agents/shared/agent-heartbeat-${AGENT_NAME:-agent}.json 2>/dev/null

# Read all heartbeats from MinIO
ROCKY=$(cat /tmp/rocky-heartbeat.json)
NATASHA=$($MC cat ${MINIO_ALIAS:-local}/agents/shared/agent-heartbeat-natasha.json 2>/dev/null || echo "null")
BULLWINKLE=$($MC cat ${MINIO_ALIAS:-local}/agents/shared/agent-heartbeat-bullwinkle.json 2>/dev/null || echo "null")

# Regenerate dashboard HTML with embedded data
QUEUE=$(cat "$QUEUE_FILE" 2>/dev/null || echo "null")
python3 /home/jkh/.openclaw/workspace/workqueue/scripts/gen-dashboard.py \
  "$ROCKY" "$NATASHA" "$BULLWINKLE" "$QUEUE" > /tmp/agent-dashboard.html 2>/dev/null

# Upload to Azure Blob
if [ -s /tmp/agent-dashboard.html ]; then
  curl -s -X PUT \
    -H "x-ms-blob-type: BlockBlob" \
    -H "Content-Type: text/html" \
    --data-binary @/tmp/agent-dashboard.html \
    "$AZURE_SAS" > /dev/null
fi
