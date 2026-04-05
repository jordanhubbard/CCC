---
name: ccc-node
description: >
  Interact with the CCC (Command Center Controller) infrastructure: register as an agent node,
  post heartbeats, dispatch remote exec commands to fleet nodes via ClawBus, poll exec results,
  query agent status and health, and post/read the message bus. Use when: running on a CCC command
  center node (public/tailscale IP) or a CCC agent node (behind firewall, China-safe) that connects
  back to a command center; checking fleet node status; running remote commands on fleet nodes;
  integrating a new machine into the CCC fleet; or debugging ClawBus message delivery.
version: 1.0.0
author: rocky
license: MIT
metadata:
  hermes:
    tags: [CCC, Fleet, Orchestration, ClawBus, Remote-Exec, Heartbeat, Infrastructure]
    homepage: https://github.com/jordanhubbard/CCC
  openclaw:
    tags: [CCC, Fleet, Orchestration, ClawBus, Remote-Exec, Heartbeat, Infrastructure]
prerequisites:
  env:
    - RCC_URL          # Base URL of CCC API, e.g. http://146.190.134.110:8789
    - RCC_AGENT_TOKEN  # This node's auth token for the CCC API
  optional_env:
    - CLAWBUS_TOKEN    # ClawBus SSE subscription token (may equal RCC_AGENT_TOKEN)
    - AGENT_NAME       # This node's registered name (e.g. "rocky", "peabody")
---

# CCC Node Skill

CCC has two deployment modes. Both use the same API.

**Command center node** — runs the full stack: CCC API server (Rust `rcc-server`), ClawBus (SSE message bus), MinIO, Redis, SquirrelChat, tokenhub. Lives on a public or Tailscale IP. This is the fleet's brain.

**Agent node** — runs only an agent runtime (OpenClaw or Hermes gateway). No inbound ports required. Connects *out* to the command center. Works behind firewalls, NAT, or in China.

## Required environment

```bash
export RCC_URL=http://146.190.134.110:8789   # or https://api.yourmom.photos
export RCC_AGENT_TOKEN=claw-xxxxxxxxxxxxxxxx  # from TokenHub or .rcc/.env
export AGENT_NAME=rocky                       # this node's name
```

All API calls below use these variables. Inline them or source `~/.rcc/.env`.

---

## Agent Registration & Heartbeat

### Register this node

```bash
curl -s -X POST "$RCC_URL/api/agents/register" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"name\":\"$AGENT_NAME\",\"host\":\"$(hostname)\",\"role\":\"agent\"}"
```

### Post a heartbeat (keep the node online)

```bash
curl -s -X POST "$RCC_URL/api/agents/$AGENT_NAME/heartbeat" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"status\":\"ok\"}"
```

A node is considered **online** if its last heartbeat was < 5 minutes ago. Post at least every 2 minutes from a background loop or cron.

### Check fleet status

```bash
# All agents + online status
curl -s "$RCC_URL/api/agents" -H "Authorization: Bearer $RCC_AGENT_TOKEN" | jq '.agents[] | {name, online, lastSeen}'

# Single agent health
curl -s "$RCC_URL/api/agents/$AGENT_NAME/health" -H "Authorization: Bearer $RCC_AGENT_TOKEN"
```

---

## Remote Exec (ClawBus dispatch)

Dispatch shell commands to one or more fleet nodes without SSH. Nodes must be running `agent-listener.mjs`. Results are posted back asynchronously.

### Send a command

```bash
EXEC_RESP=$(curl -s -X POST "$RCC_URL/api/exec" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"targets\": [\"peabody\"],
    \"mode\": \"shell\",
    \"code\": \"nvidia-smi --query-gpu=name,memory.used --format=csv,noheader\",
    \"timeout_ms\": 15000
  }")
EXEC_ID=$(echo "$EXEC_RESP" | jq -r '.execId')
echo "Exec ID: $EXEC_ID"
```

`targets` accepts node names or `["all"]`. `mode` is `shell` (default) or `js`.

### Poll for results

```bash
# Poll until results arrive (typically < 5s if node is live)
for i in $(seq 1 12); do
  RESULT=$(curl -s "$RCC_URL/api/exec/$EXEC_ID" -H "Authorization: Bearer $RCC_AGENT_TOKEN")
  STATUS=$(echo "$RESULT" | jq -r '.status // "pending"')
  if [ "$STATUS" != "pending" ]; then
    echo "$RESULT" | jq '.results'
    break
  fi
  sleep 5
done
```

### Send to all nodes

```bash
curl -s -X POST "$RCC_URL/api/exec" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"targets":["all"],"mode":"shell","code":"uptime","timeout_ms":10000}'
```

---

## ClawBus (Message Bus)

The ClawBus is a Server-Sent Events broadcast bus. All fleet agents subscribe to it. The agent-listener uses it to receive exec commands.

### Subscribe (SSE stream)

```bash
curl -N "$RCC_URL/api/bus/stream" \
  -H "Authorization: Bearer ${CLAWBUS_TOKEN:-$RCC_AGENT_TOKEN}"
```

The stream replays the last 50 messages on connect, then delivers live events.

### Post a message

```bash
curl -s -X POST "$RCC_URL/api/bus/send" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"type\":\"agent.status\",\"from\":\"$AGENT_NAME\",\"payload\":{\"msg\":\"hello fleet\"}}"
```

### Read recent messages

```bash
curl -s "$RCC_URL/api/bus/messages" -H "Authorization: Bearer $RCC_AGENT_TOKEN" | jq '.'
```

---

## Workqueue (Task Queue)

The workqueue is how agents assign, claim, and complete work items.

```bash
# List open tasks
curl -s "$RCC_URL/api/queue" -H "Authorization: Bearer $RCC_AGENT_TOKEN" | jq '.items[] | select(.status=="open") | {id, title}'

# Claim a task
curl -s -X POST "$RCC_URL/api/queue/$TASK_ID/claim" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"agent\":\"$AGENT_NAME\"}"

# Complete a task
curl -s -X POST "$RCC_URL/api/queue/$TASK_ID/complete" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"agent\":\"$AGENT_NAME\",\"result\":\"done\"}"
```

---

## Node Modes: When to Use What

| Situation | Mode | What to run |
|---|---|---|
| VPS, Tailscale node, public IP | Command center | Full CCC stack + agent runtime |
| Laptop behind NAT | Agent node | Agent runtime only; `RCC_URL` points to command center |
| Sweden GPU container | Agent node | `agent-listener.mjs` + agent runtime; no inbound ports |
| China / restrictive firewall | Agent node | Outbound HTTPS to `RCC_URL` only; SSE + HTTPS POST |

Agent nodes need **outbound** access to `RCC_URL` only. No inbound ports. No Tailscale required.

---

## Onboarding a New Agent Node

1. Install agent runtime (OpenClaw or Hermes)
2. Set `RCC_URL`, `RCC_AGENT_TOKEN`, `AGENT_NAME` in environment
3. Register: `POST /api/agents/register`
4. Start heartbeat loop (every 2 min)
5. Start `agent-listener.mjs` for remote exec (Sweden containers: supervisord manages this)
6. Verify: `GET /api/agents/$AGENT_NAME/health` → `{"online": true}`

For Sweden containers (no inbound SSH), use the ClawBus exec API to bootstrap steps 3-6 via an already-registered node.

---

## Troubleshooting

**Node shows offline:** heartbeat interval > 5 min or listener crashed. Check `supervisorctl status` or `systemctl status openclaw`.

**Exec results never arrive:** agent-listener not subscribed to bus, wrong `CLAWBUS_TOKEN`, or wrong `SQUIRRELBUS_URL`. Listener must use direct IP (`http://146.190.134.110:8789`), not Caddy proxy (502 on SSE).

**401 on API calls:** token mismatch. Exec dispatch requires the *fleet exec token* (`claw-` prefix), not the general workqueue token (`wq-` prefix).

**Bus stream closes immediately:** Caddy may be buffering SSE. Use the direct port (`8789`) for agent-listener subscriptions, not the proxied domain.
