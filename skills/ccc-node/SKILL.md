---
name: ccc-node
description: Connect this agent to the CCC (Command and Control Center) fleet. Handles ClawBus registration, heartbeat, remote exec dispatch, and workqueue lifecycle. Use when setting up a new agent node, checking fleet connectivity, or managing workqueue items via Rocky's RCC API.
version: 1.0.0
platforms: [linux, macos]
metadata:
  hermes:
    tags: [ccc, clawbus, fleet, rcc, workqueue]
    category: infrastructure
required_environment_variables:
  - name: CCC_URL
    prompt: "RCC API base URL (e.g. http://146.190.134.110:8789)"
    help: "Rocky's RCC server URL. Internal agents use http://146.190.134.110:8789 directly."
    required_for: all CCC operations
  - name: CCC_AGENT_TOKEN
    prompt: "CCC agent bearer token (rcc-agent-<name>-<hex>)"
    help: "Pull from Rocky's secrets store: GET /api/secrets/<agentname>_ccc_token"
    required_for: authenticated API calls
  - name: AGENT_NAME
    prompt: "This agent's name (e.g. bullwinkle, natasha)"
    help: "Lowercase, matches the name registered in Rocky's RCC fleet."
    required_for: heartbeat and workqueue routing
---

# CCC Node

Connects a Hermes agent to the CCC fleet running on Rocky (do-host1, 146.190.134.110).

CCC = the Command and Control Center. Rocky runs the RCC server (`rcc-server`, Rust/Axum).
ClawBus is the SquirrelBus-based message bus. All fleet coordination goes through Rocky.

## When to Use

- First-time setup of a new agent node on the fleet
- Checking whether this agent is registered and heartbeating
- Pulling or completing workqueue items
- Sending or receiving ClawBus messages
- Diagnosing connectivity to Rocky

## Architecture

```
Agent (you) ──HTTP──▶ Rocky RCC API (http://146.190.134.110:8789)
                         ├── /api/heartbeat/<name>    POST — heartbeat
                         ├── /api/workqueue           GET — pull items
                         ├── /api/workqueue/<id>      PATCH — update status
                         ├── /api/bus/send            POST — ClawBus message
                         ├── /api/exec/<id>/result    POST — exec result
                         └── /api/secrets/<key>       GET — secrets store
```

All requests require `Authorization: Bearer $CCC_AGENT_TOKEN`.

**Network note:** Rocky's RCC API is NOT on the public internet — it's on the internal interface.
- From puck (Bullwinkle): reach via Tailscale (`http://100.89.199.14:8789`) or direct IP if routed
- From Sweden containers: they have no inbound; they connect outbound via SSH tunnel to Rocky
- From sparky (Natasha): Tailscale IP works

## Procedure

### 1. Verify connectivity

```bash
curl -s -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  $CCC_URL/api/health | jq .
```

Expected: `{"status":"ok",...}`

### 2. Send a heartbeat

```bash
curl -s -X POST \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"agent\":\"$AGENT_NAME\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" \
  $CCC_URL/api/heartbeat/$AGENT_NAME
```

Set this up as a cron job every 60s:
```
hermes cron create "Send CCC heartbeat" --interval 60s --quiet \
  --task "Run the CCC heartbeat curl command using env vars CCC_URL, CCC_AGENT_TOKEN, AGENT_NAME"
```

### 3. Pull workqueue items

```bash
curl -s -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  "$CCC_URL/api/workqueue?assignee=$AGENT_NAME&status=pending" | jq .
```

Items have: `id`, `title`, `description`, `assignee`, `status`, `priority`, `created_at`

### 4. Update workqueue item status

```bash
# Mark in_progress
curl -s -X PATCH \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"status":"in_progress"}' \
  $CCC_URL/api/workqueue/<item-id>

# Mark completed
curl -s -X PATCH \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"status":"completed","result":"summary of what was done"}' \
  $CCC_URL/api/workqueue/<item-id>
```

### 5. Send a ClawBus message

```bash
curl -s -X POST \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"from\":\"$AGENT_NAME\",\"to\":\"rocky\",\"type\":\"message\",\"payload\":{\"text\":\"Hello from $AGENT_NAME\"}}" \
  $CCC_URL/api/bus/send
```

### 6. Respond to a remote exec

If you're running `agent-listener.mjs`, exec results post back automatically.
Manual result post:
```bash
curl -s -X POST \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"result":"output here","exitCode":0}' \
  $CCC_URL/api/exec/<exec-id>/result
```

### 7. Fetch a secret from Rocky's store

```bash
curl -s -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  $CCC_URL/api/secrets/<secret-key>
```

Common keys:
- `bullwinkle_ccc_token` — Bullwinkle's CCC bearer token
- `minio_access_key` / `minio_secret_key` — MinIO credentials
- `slack_bot_token_<name>` — Slack tokens for Sweden fleet agents

### 8. Register with the fleet (first run)

Tell Rocky you exist:
```bash
curl -s -X POST \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"name\":\"$AGENT_NAME\",\"runtime\":\"hermes\",\"host\":\"$(hostname)\",\"capabilities\":[\"general\",\"coding\",\"browser\"]}" \
  $CCC_URL/api/agents/register
```

## Hermes-specific wiring

Add to `~/.hermes/config.yaml`:
```yaml
env:
  CCC_URL: "http://100.89.199.14:8789"   # Tailscale IP for puck
  CCC_AGENT_TOKEN: "<your token>"
  AGENT_NAME: "bullwinkle"
```

Or export in your shell profile and let `hermes claw migrate` carry them over.

**ClawBus plugin note:** OpenClaw had a native ClawBus plugin. Hermes doesn't — this skill is
the replacement. Use the curl commands above, or wrap them in a Hermes hook script if you want
automatic polling. The agent-listener daemon (`agent-listener.mjs` in the CCC repo) handles
inbound exec dispatch independently of the agent runtime.

## Pitfalls

- **Wrong token type:** Use `rcc-agent-*` tokens, NOT `wq-*` workqueue tokens. They're different.
- **Snidely token typo:** Rocky's fleet has `rcc-agent-Snidley` (note "Snidley" vs "Snidely") — use it as-is.
- **ClawBus SSE from Sweden:** Use direct IP `http://146.190.134.110:8789`, NOT the Caddy proxy URL — Caddy returns 502 on SSE endpoints.
- **puck networking:** puck has Tailscale, so use `http://100.89.199.14:8789` (Tailscale IP for do-host1).
- **sessions_spawn / sessions_yield:** These are OpenClaw-specific. In Hermes, use `delegate_tool.py` or spawn subagents via the Hermes delegate API.

## Verification

```bash
# Health check
curl -s $CCC_URL/api/health

# Confirm you appear in the fleet
curl -s -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  $CCC_URL/api/agents | jq '.[] | select(.name == env.AGENT_NAME)'

# Check recent heartbeats in dashboard
# https://dashboard.yourmom.photos → Fleet tab
```
