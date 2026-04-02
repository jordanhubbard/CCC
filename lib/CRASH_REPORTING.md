# Crash Reporting for CCC Services

Automatic crash detection and task filing for Node.js services.

## Quick Start

Add two lines near the top of any Node.js service (after imports, before everything else):

```js
import { initCrashReporter } from '../lib/crash-reporter.mjs';
initCrashReporter({
  service: 'your-service-name',
  sourceDir: '~/.openclaw/workspace/your-service'
});
```

That's it. On any uncaught exception or unhandled rejection, the crash reporter will:

1. **Upload a crash log to MinIO** at `agents/logs/<service>-crash-<timestamp>.json`
2. **POST to the dashboard API** at `http://localhost:8788/api/crash-report`
3. **Fall back to direct queue.json write** if the API is unavailable
4. **Exit the process** (so systemd or your process manager can restart it)

## How It Works

### Node.js Handler (`crash-reporter.mjs`)

Registers `process.on('uncaughtException')` and `process.on('unhandledRejection')` handlers.

On crash:
- Writes a detailed crash log (error, stack, pid, memory, uptime) to MinIO
- Files a high-priority task in the workqueue
- If the crashing service IS the dashboard, writes directly to `queue.json` (can't POST to yourself when you're dead)

### Dashboard API Endpoint

```
POST http://localhost:8788/api/crash-report
Authorization: Bearer <your-rcc-token>
Content-Type: application/json

{
  "service": "wq-api",
  "error": "ECONNREFUSED",
  "stack": "Error: ECONNREFUSED\n    at ...",
  "sourceDir": "~/.openclaw/workspace/workqueue",
  "ts": "1711065600000"
}

Response: { "ok": true, "taskId": "wq-crash-1711065600000" }
```

### systemd Crash Hook (`systemd-crash-hook.sh`)

For services managed by systemd, add to the `.service` file:

```ini
[Service]
ExecStopPost=~/.openclaw/workspace/lib/systemd-crash-hook.sh <service-name>
```

The hook:
- Only fires on non-zero exit (clean stops are ignored)
- Captures the last 20 lines of journal output
- Uploads crash log to MinIO
- Tries the dashboard API first, falls back to direct `queue.json` write

This provides a second layer of crash detection — even if the Node.js handler fails to fire (e.g., segfault, OOM kill), systemd will still report the crash.

## Crash Task Format

```json
{
  "id": "wq-crash-<timestamp>",
  "itemVersion": 1,
  "created": "<ISO>",
  "source": "system",
  "assignee": "all",
  "priority": "high",
  "status": "pending",
  "title": "CRASH: <service> — <error message, truncated to 80 chars>",
  "description": "Unhandled exception in <service>. Stack trace and logs available.",
  "notes": "Error: <full message>\nStack: <first 5 lines>\nSource: <sourceDir>\nMinIO logs: agents/logs/<service>-crash-<timestamp>.json",
  "tags": ["crash", "auto-filed", "<service>"],
  "channel": "mattermost",
  "claimedBy": null,
  "claimedAt": null,
  "attempts": 0,
  "maxAttempts": 1,
  "lastAttempt": null,
  "completedAt": null,
  "result": null
}
```

## MinIO Log Convention

All crash logs go to:
```
$MINIO_ALIAS/agents/logs/<service>-crash-<timestamp>.json
```

Where `<timestamp>` is `Date.now()` (milliseconds since epoch).

To browse crash logs:
```bash
mc ls $MINIO_ALIAS/agents/logs/ | grep crash
```

To read a specific crash log:
```bash
mc cat $MINIO_ALIAS/agents/logs/wq-dashboard-crash-1711065600000.json | jq .
```

## Finding and Fixing Crash Tasks

### In the Dashboard
1. Open the [Claw Command Center](http://localhost:8788/)
2. Filter by "Pending" — crash tasks show up as high priority with `CRASH:` prefix
3. Check the task notes for error details, stack trace, and MinIO log path
4. Fix the issue, then mark the task as complete

### Via API
```bash
# List all crash tasks
curl http://localhost:8788/api/queue | jq '.items[] | select(.tags[]? == "crash")'
```

### Via queue.json directly
```bash
cat ~/.openclaw/workspace/workqueue/queue.json | jq '.items[] | select(.tags[]? == "crash") | {id, title, status}'
```

## Currently Wired Services

| Service | Type | Crash Reporter | systemd Hook |
|---------|------|---------------|--------------|
| wq-dashboard | systemd | ✅ `crash-reporter.mjs` | ✅ `systemd-crash-hook.sh` |
| wq-api | background | (wire it up!) | N/A |

## Adding Crash Reporting to a New Service

1. **Node.js services**: Add the import + `initCrashReporter()` call (see Quick Start)
2. **systemd services**: Add `ExecStopPost=` line to the `.service` file, then `sudo systemctl daemon-reload`
3. **Both**: Do both for belt-and-suspenders coverage

## Testing

```bash
node ~/.openclaw/workspace/lib/test-crash-reporter.mjs
```

This deliberately throws an unhandled exception after 1 second. Check `queue.json` for a new crash task tagged `test-crash-reporter`.
