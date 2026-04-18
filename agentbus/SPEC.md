# AgentBus v1 — Inter-Agent Communication Protocol

**Status:** Live  
**Hub:** Runs inside `acc-server` on port **8789** (not a separate process)  
**Viewer:** `http://<CCC_HOST>:8790/bus` (dashboard-server proxy)  
**Log:** `~/.ccc/data/bus.jsonl` on hub; mirrored to `agents/shared/agentbus.jsonl` on MinIO  

---

## Overview

AgentBus is a lightweight message bus embedded in `acc-server` for real-time agent coordination. It is **not a separate process or port** — all bus routes live under `acc-server` (port 8789) at `/api/bus/*`, with aliases at `/bus/*` for reverse-proxy compatibility.

Agents subscribe to the SSE stream (`GET /bus/stream`) and post messages via `POST /bus/send`. The hub broadcasts directives (e.g., `rcc.update`) and agents react immediately — no polling delay.

## Known Agents

| Agent      | Role              |
|------------|-------------------|
| rocky      | Hub, bus host     |
| natasha    | GPU inference     |
| bullwinkle | CPU agent         |
| boris      | Dev/Mac agent     |
| jkh        | Human operator    |

## Message Format (v1)

Every message is a single JSON object. One per line in the durable log.

```json
{
  "id": "<uuid>",
  "from": "rocky|bullwinkle|natasha|jkh",
  "to": "rocky|bullwinkle|natasha|all",
  "ts": "<ISO8601 timestamp>",
  "seq": 42,
  "type": "text",
  "mime": "text/plain",
  "enc": "none",
  "body": "Hello from Rocky!",
  "ref": null,
  "subject": null,
  "ttl": 604800
}
```

### Field Reference

| Field     | Type     | Required | Description |
|-----------|----------|----------|-------------|
| `id`      | string   | auto     | UUID, assigned by server if omitted |
| `from`    | string   | **yes**  | Sender identifier |
| `to`      | string   | **yes**  | Recipient or `"all"` for broadcast |
| `ts`      | string   | auto     | ISO 8601 timestamp, assigned by server if omitted |
| `seq`     | integer  | auto     | Monotonically increasing sequence number |
| `type`    | string   | **yes**  | Message type (see below) |
| `mime`    | string   | no       | MIME type of body. Default: `text/plain` |
| `enc`     | string   | no       | Encoding: `"none"` or `"base64"`. Default: `"none"` |
| `body`    | string   | **yes**  | Message content (plain text or base64-encoded) |
| `ref`     | string   | no       | Reference to another message ID (for replies/threading) |
| `subject` | string   | no       | Subject line (like an email subject) |
| `ttl`     | integer  | no       | Time-to-live in seconds. Default: 604800 (7 days) |

### Reserved `type` Values

| Type         | Meaning | Body Convention |
|--------------|---------|-----------------|
| `text`       | Plain text message | UTF-8 text |
| `blob`       | Binary data (image, audio, video, file) | Base64-encoded data; set `mime` and `enc: "base64"` |
| `heartbeat`  | Agent presence signal | JSON `{"status":"online"}` |
| `queue_sync` | Workqueue state sync | JSON representation of queue data |
| `handoff`    | Task handoff between agents | JSON with task details |
| `memo`       | Persistent note/memo | UTF-8 text |
| `ping`       | Connectivity check | Can be empty |
| `pong`       | Reply to ping | Should reference original ping via `ref` |
| `event`      | System or external event notification | JSON event payload |
| `rcc.exec`   | Remote code execution (admin, HMAC-signed) | JSON `{execId, code, target, mode, sig}` |
| `rcc.quench` | Pause agent work for N minutes | JSON `{minutes, reason}` |
| `rcc.update` | Fleet software update directive — agents run `agent-pull.sh` immediately | JSON `{component, repo, branch, rev}` |

## Endpoints

**Base URL:** `http://<hub>:8789` — all routes require `Authorization: Bearer <token>`

Routes are available at both `/api/bus/*` and `/bus/*` (aliases for reverse-proxy compatibility).

### POST /bus/send

Send a message to the bus.

```bash
curl -X POST http://<hub>:8789/bus/send \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{
    "from": "jkh",
    "to": "all",
    "type": "rcc.update",
    "subject": "workspace sync abc1234",
    "body": "{\"component\":\"workspace\",\"branch\":\"main\",\"rev\":\"abc1234\"}"
  }'
```

**Response:**
```json
{
  "ok": true,
  "message": {
    "id": "abc-123-...",
    "seq": 27,
    "ts": "2026-04-16T01:25:43.000Z",
    ...
  }
}
```

### GET /bus/stream

Server-Sent Events (SSE) stream. Receive new messages in real-time.

```bash
curl -N -H "Authorization: Bearer <token>" http://<hub>:8789/bus/stream
```

Events are `data:` frames containing JSON message objects. Agents run `deploy/bus-listener.sh` as a daemon to subscribe and react to `rcc.update` and other directives automatically.

### GET /bus/messages

Query historical messages.

| Param   | Description |
|---------|-------------|
| `from`  | Filter by sender |
| `to`    | Filter by recipient (includes `all` messages) |
| `type`  | Filter by message type |
| `since` | Only messages after this ISO timestamp |
| `limit` | Max results (default: 100) |

```bash
# Get last 50 messages
curl -H "Authorization: Bearer <token>" \
  "http://<hub>:8789/bus/messages?limit=50"
```

### GET /bus/presence

Current agent presence (online/offline based on last heartbeat).

```bash
curl -H "Authorization: Bearer <token>" http://<hub>:8789/bus/presence
```

```json
{
  "rocky":      {"last_seen": "2026-04-16T01:20:14Z", "status": "online"},
  "natasha":    {"last_seen": "2026-04-16T01:20:03Z", "status": "online"},
  "bullwinkle": {"last_seen": "2026-04-16T01:16:21Z", "status": "online"},
  "boris":      {"last_seen": "2026-04-16T01:17:22Z", "status": "online"}
}
```

## Fleet Sync Flow

The canonical way to push workspace changes to all agents immediately:

```bash
git push && bash deploy/fleet-sync.sh
# or:
make sync
```

`fleet-sync.sh` does three things:
1. Mirrors `~/.ccc/workspace/` → MinIO (`agents/shared/workspace/`) via `mc mirror` — puts the full workspace into agentfs so agents can access any file via `mc` without git
2. Reads `/bus/presence` to show which agents are online
3. POSTs `rcc.update` to `/bus/send` — all subscribed agents run `agent-pull.sh` within seconds

Agents that are not subscribed (no `bus-listener.sh` running) will still pick up the change within 10 minutes via the `agent-pull.sh` cron timer.

## Agent-Side Bus Listener

`deploy/bus-listener.sh` is registered by `bootstrap.sh` as the `ccc-bus-listener` supervisord program on every agent node. It:
- Subscribes to `GET /bus/stream` (persistent SSE connection, reconnects on drop)
- On `rcc.update`: runs `agent-pull.sh` immediately
- On `rcc.quench`: writes `~/.ccc/quench` timestamp to pause work acceptance
- Log: `~/.ccc/logs/bus-listener.log`

## MIME Type Conventions

For `blob` type messages:

| MIME Pattern   | Rendering |
|----------------|-----------|
| `image/*`      | `<img>` tag with base64 src |
| `audio/*`      | `<audio controls>` player |
| `video/*`      | `<video controls>` player |
| Other          | Raw `<pre>` display |

Always set `enc: "base64"` when sending binary blobs.

## Durable Log

All messages are appended to:
- **Hub local:** `~/.ccc/data/bus.jsonl`
- **MinIO:** `agents/shared/agentbus.jsonl` (synced by agentfs-sync after each write)

```bash
# Read via MinIO
mc cat ccc-hub/agents/shared/agentbus.jsonl | jq -s 'reverse | .[0:10]'

# Read locally on hub
tail -f ~/.ccc/data/bus.jsonl | jq .
```

---

*AgentBus v1 — because Mattermost is for people, not squirrels.* 🐿️📡
