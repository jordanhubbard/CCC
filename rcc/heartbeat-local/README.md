# heartbeat-local

Offline heartbeat buffer for sparky. Stores agent heartbeats in SQLite when
CCC (do-host1:8789) is unreachable, and replays them when connectivity resumes.

## Problem

When sparky can't reach CCC, agents appear dead on the dashboard even though
they're running fine. This makes it impossible to distinguish network partition
from a real agent failure.

## Solution

Run `heartbeat-local` on sparky (port 8790). Agents post to it when CCC fails.
It buffers in SQLite and replays on reconnect. The WASM dashboard can poll
`GET /local/heartbeat` to show accurate liveness even during CCC downtime.

## Endpoints

| Method | Path | Description |
|---|---|---|
| `POST /api/heartbeat/:agent` | Drop-in for CCC — stores locally, forwards if CCC up |
| `GET /local/heartbeat` | All agents with last-seen, CCC reachability state |
| `GET /health` | `{ok:true, rcc_reachable:bool}` |

## Usage

```bash
# Install dependency (optional, falls back to in-memory if absent)
cd rcc/heartbeat-local && npm install better-sqlite3

# Run
node server.mjs

# Or with env vars
HB_LOCAL_PORT=8790 CCC_URL=http://146.190.134.110:8789 \
CCC_AUTH_TOKEN=wq-xxx node server.mjs
```

## Agent heartbeat script

In each agent's heartbeat, try CCC first, fall back to local:

```bash
RCC_OK=$(curl -sf -X POST $CCC_URL/api/heartbeat/$AGENT_NAME \
  -H "Authorization: Bearer $CCC_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"status":"online","host":"sparky","ts":"'"$(date -u +%Y-%m-%dT%H:%M:%SZ)"'"}')

if [ -z "$RCC_OK" ]; then
  # Fallback to local buffer
  curl -sf -X POST http://localhost:8790/api/heartbeat/$AGENT_NAME \
    -H "Content-Type: application/json" \
    -d '{"status":"online","host":"sparky","ts":"'"$(date -u +%Y-%m-%dT%H:%M:%SZ)"'"}'
fi
```

## Systemd

See `../../deploy/heartbeat-local.service`.
