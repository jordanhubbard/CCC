#!/bin/bash
# systemd-crash-hook.sh — Post-stop crash reporter for systemd services
#
# Usage in .service file:
#   ExecStopPost=${OPENCLAW_WORKSPACE:-~/.openclaw/workspace}/lib/systemd-crash-hook.sh <service-name>
#
# Only fires if the service exited with a non-zero exit code.
# Writes a crash task to queue.json and uploads a crash log to MinIO.

SERVICE="${1:-unknown}"
TS=$(date +%s%3N)
MC="mc"
MINIO_ALIAS="${MINIO_ALIAS:-local}"
QUEUE_PATH="${OPENCLAW_WORKSPACE:-~/.openclaw/workspace}/workqueue/queue.json"
DASHBOARD_URL="http://localhost:8788/api/crash-report"
AUTH_TOKEN="${CCC_AGENT_TOKEN:-}"
LOG_DIR="/tmp"

# systemd sets these env vars for ExecStopPost:
# $EXIT_CODE - "exited" or "killed"
# $EXIT_STATUS - exit code number or signal name
# $SERVICE_RESULT - "success", "exit-code", "signal", etc.

# Only act on actual crashes (non-zero exit)
if [ "$SERVICE_RESULT" = "success" ]; then
    exit 0
fi

# Also check EXIT_STATUS — if it's 0, this was a clean stop
if [ "$EXIT_STATUS" = "0" ] && [ "$EXIT_CODE" = "exited" ]; then
    exit 0
fi

echo "[crash-hook] 💥 ${SERVICE} crashed: EXIT_CODE=${EXIT_CODE}, EXIT_STATUS=${EXIT_STATUS}, SERVICE_RESULT=${SERVICE_RESULT}"

# Grab recent journal logs for context
JOURNAL_LINES=$(journalctl -u "${SERVICE}" -n 20 --no-pager 2>/dev/null || echo "No journal access")

# Build crash log JSON
CRASH_LOG="${LOG_DIR}/crash-${SERVICE}-${TS}.json"
cat > "$CRASH_LOG" <<EOJSON
{
  "service": "${SERVICE}",
  "exitCode": "${EXIT_CODE}",
  "exitStatus": "${EXIT_STATUS}",
  "serviceResult": "${SERVICE_RESULT}",
  "timestamp": "${TS}",
  "reportedAt": "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)",
  "source": "systemd-crash-hook",
  "journalTail": $(echo "$JOURNAL_LINES" | python3 -c "import sys,json; print(json.dumps(sys.stdin.read()))" 2>/dev/null || echo '"no journal"')
}
EOJSON

# Upload crash log to MinIO
MINIO_PATH="${MINIO_ALIAS}/agents/logs/${SERVICE}-crash-${TS}.json"
$MC cp "$CRASH_LOG" "$MINIO_PATH" 2>/dev/null && echo "[crash-hook] Uploaded crash log to ${MINIO_PATH}" || echo "[crash-hook] MinIO upload failed"

# Try the dashboard API first
API_RESPONSE=$(curl -s -w "\n%{http_code}" -X POST "$DASHBOARD_URL" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer ${AUTH_TOKEN}" \
  -d "{\"service\":\"${SERVICE}\",\"error\":\"Process ${EXIT_CODE} with status ${EXIT_STATUS} (${SERVICE_RESULT})\",\"stack\":\"systemd ExecStopPost hook\\nEXIT_CODE=${EXIT_CODE}\\nEXIT_STATUS=${EXIT_STATUS}\\nSERVICE_RESULT=${SERVICE_RESULT}\",\"sourceDir\":\"${OPENCLAW_WORKSPACE:-~/.openclaw/workspace}\",\"ts\":\"${TS}\"}" \
  --connect-timeout 3 --max-time 5 2>/dev/null)

HTTP_CODE=$(echo "$API_RESPONSE" | tail -1)

if [ "$HTTP_CODE" = "200" ]; then
    echo "[crash-hook] Crash report filed via dashboard API"
else
    echo "[crash-hook] Dashboard API unavailable (HTTP ${HTTP_CODE}), writing directly to queue.json"
    
    # Direct write to queue.json as fallback, with burst dedup
    node -e "
const fs = require('fs');
const qPath = '${QUEUE_PATH}';
const svc = '${SERVICE}';
const BURST_WINDOW_MS = 60_000;
const BURST_MAX = 3;
try {
  const data = JSON.parse(fs.readFileSync(qPath, 'utf8'));
  data.items = data.items || [];
  const now = Date.now();
  // Dedup: count recent crash items for this service still pending
  const recentCrashes = data.items.filter(i =>
    i.id && i.id.startsWith('wq-crash-') &&
    Array.isArray(i.tags) && i.tags.includes(svc) &&
    i.status === 'pending' &&
    (now - new Date(i.created || 0).getTime()) < BURST_WINDOW_MS
  );
  if (recentCrashes.length >= BURST_MAX) {
    console.log('[crash-hook] Burst suppressed — ' + recentCrashes.length + ' recent crash items for ' + svc + ' already pending');
    process.exit(0);
  }
  data.items.push({
    id: 'wq-crash-${TS}',
    itemVersion: 1,
    created: new Date(${TS}).toISOString(),
    source: 'system',
    assignee: 'all',
    priority: 'high',
    status: 'pending',
    title: 'CRASH: ${SERVICE} — Process ${EXIT_CODE} with status ${EXIT_STATUS}',
    description: 'Unhandled crash in ${SERVICE} detected by systemd. Journal logs available.',
    notes: 'EXIT_CODE=${EXIT_CODE}\\nEXIT_STATUS=${EXIT_STATUS}\\nSERVICE_RESULT=${SERVICE_RESULT}\\nMinIO logs: agents/logs/${SERVICE}-crash-${TS}.json',
    tags: ['crash', 'auto-filed', '${SERVICE}'],
    channel: 'mattermost',
    claimedBy: null, claimedAt: null, attempts: 0, maxAttempts: 1,
    lastAttempt: null, completedAt: null, result: null
  });
  data.lastSync = new Date().toISOString();
  fs.writeFileSync(qPath, JSON.stringify(data, null, 2) + '\\n', 'utf8');
  console.log('[crash-hook] Wrote crash task to queue.json');
} catch(e) {
  console.error('[crash-hook] Failed to write queue.json:', e.message);
}
" 2>&1
fi

# Cleanup temp file
rm -f "$CRASH_LOG"

echo "[crash-hook] Done."
