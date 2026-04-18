---
name: ccc-node
description: Connect this agent to the CCC (Command and Control Center) fleet. Handles ClawBus registration, heartbeat, remote exec dispatch, and workqueue lifecycle. Use when setting up a new agent node, checking fleet connectivity, or managing workqueue items via the CCC API.
version: 1.0.0
platforms: [linux, macos]
metadata:
  hermes:
    tags: [ccc, clawbus, fleet, ccc, workqueue]
    category: infrastructure
required_environment_variables:
  - name: CCC_URL
    prompt: "CCC API base URL (e.g. http://<hub-ip>:8789 or Tailscale URL from CCC_TAILSCALE_URL)"
    help: "Set to the URL your CCC admin provided. Check ~/.ccc/.env for CCC_URL. If unreachable, try CCC_TAILSCALE_URL."
    required_for: all CCC operations
  - name: CCC_AGENT_TOKEN
    prompt: "CCC agent bearer token (ccc-agent-<name>-<hex>)"
    help: "Provided by your CCC admin at onboarding. Stored in ~/.ccc/.env as CCC_AGENT_TOKEN."
    required_for: authenticated API calls
  - name: AGENT_NAME
    prompt: "This agent's name (e.g. bullwinkle, natasha)"
    help: "Lowercase, matches the name registered in the CCC fleet."
    required_for: heartbeat and workqueue routing
---

# CCC Node

Connects a Hermes agent to the CCC fleet.

CCC = the Command and Control Center. The hub runs `acc-server` (Rust/Axum) on port 8789.
ClawBus is the inter-agent message bus. All fleet coordination goes through the hub.

## When to Use

- First-time setup of a new agent node on the fleet
- Checking whether this agent is registered and heartbeating
- Pulling or completing workqueue items
- Sending or receiving ClawBus messages
- Diagnosing connectivity to the hub

## Architecture

```
Agent (you) ──HTTP──▶ CCC Hub ($CCC_URL)
                         ├── /api/heartbeat/<name>    POST — heartbeat
                         ├── /api/workqueue           GET — pull items
                         ├── /api/workqueue/<id>      PATCH — update status
                         ├── /api/bus/send            POST — ClawBus message
                         ├── /api/exec/<id>/result    POST — exec result
                         └── /api/secrets/<key>       GET — secrets store
```

All requests require `Authorization: Bearer $CCC_AGENT_TOKEN`.

**Network note:** The hub may not be on the public internet. Check your `~/.ccc/.env`:
- `CCC_URL` — primary hub URL (may be a public IP, private IP, or Tailscale address)
- `CCC_TAILSCALE_URL` — optional Tailscale fallback URL; `ccc-connectivity-check.sh` will failover to this automatically if the primary is unreachable

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

### 7. Fetch a secret from the hub's store

```bash
curl -s -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  $CCC_URL/api/secrets/<secret-key>
```

Common keys:
- `<agentname>_ccc_token` — agent's CCC bearer token
- `minio_access_key` / `minio_secret_key` — MinIO credentials
- `slack_bot_token_<name>` — Slack tokens for agents

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

The required env vars (`CCC_URL`, `CCC_AGENT_TOKEN`, `AGENT_NAME`) are loaded from `~/.ccc/.env`
automatically when hermes-agent starts via `agent-pull.sh`. No manual config needed after setup.

**ClawBus plugin note:** Use the curl commands above, or wrap them in a Hermes hook script for
automatic polling. The `ccc-agent listen` daemon handles inbound exec dispatch independently
of the agent runtime.

## Task Workspace (AgentFS workflow)

Every queue-worker task runs with an isolated workspace. These environment variables
are always set when you are executing a task:

| Variable | Value | Purpose |
|---|---|---|
| `TASK_ID` | e.g. `wq-ROC-idea-20260416-abc` | Current task identifier |
| `TASK_WORKSPACE_LOCAL` | `~/.ccc/task-workspaces/<id>/` | Your working directory |
| `TASK_WORKSPACE_AGENTFS` | `ccc-hub/agents/tasks/<id>/workspace` | AgentFS mirror (MinIO) |
| `TASK_BRANCH` | `task/<id>` | Git branch for the single push |

### Rules — enforced, not advisory

1. **Work only inside `$TASK_WORKSPACE_LOCAL`.** All file edits, builds, and test runs
   must happen within this directory. It is a fresh `git clone` of the task's project.

2. **Never run `git commit` or `git push` yourself.** The queue-worker calls
   `task-workspace-finalize.sh` after you complete the task, which stages all your
   changes, commits them, and does exactly one `git push` to `$TASK_BRANCH`.
   Any commit or push you make during the task will create conflicts or duplicate history.

3. **Never clone repos yourself during task execution.** The workspace is already
   cloned from the correct repo and branch. If you need a dependency, install it
   inside the workspace, don't clone another copy.

4. **Signal completion by producing your final summary output.** The queue-worker
   detects exit 0 as success and triggers finalization. Do not try to call the
   CCC `/complete` endpoint yourself — the queue-worker handles that too.

5. **The workspace is ephemeral.** It is deleted after finalization. Store all
   durable output as file changes inside `$TASK_WORKSPACE_LOCAL`.

### Verifying your workspace

```bash
echo "Task:      $TASK_ID"
echo "Workspace: $TASK_WORKSPACE_LOCAL"
echo "AgentFS:   $TASK_WORKSPACE_AGENTFS"
echo "Branch:    $TASK_BRANCH"
ls "$TASK_WORKSPACE_LOCAL"
git -C "$TASK_WORKSPACE_LOCAL" log --oneline -5
```

### If you need to access AgentFS directly

```bash
# List workspace contents in MinIO
mc ls "$TASK_WORKSPACE_AGENTFS/"

# Check task metadata
MC_ALIAS=$(echo "$TASK_WORKSPACE_AGENTFS" | cut -d/ -f1)
BUCKET=$(echo "$TASK_WORKSPACE_AGENTFS" | cut -d/ -f2)
mc cat "${MC_ALIAS}/${BUCKET}/tasks/${TASK_ID}/meta.json" | jq .
```

## Pitfalls

- **Wrong token type:** Use `ccc-agent-*` tokens, NOT `wq-*` workqueue tokens. They're different.
- **ClawBus SSE:** Use `$CCC_URL` directly, not a proxy URL — proxies may return 502 on SSE endpoints.
- **sessions_spawn / sessions_yield:** These are OpenClaw-specific. In Hermes, use `delegate_tool.py` or spawn subagents via the Hermes delegate API.
- **git during task:** Do not commit or push. Any changes you make inside `$TASK_WORKSPACE_LOCAL` will be committed by the queue-worker on completion. Committing yourself creates duplicate history on the task branch.

## Verification

```bash
# Health check
curl -s $CCC_URL/api/health

# Confirm you appear in the fleet
curl -s -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  $CCC_URL/api/agents | jq '.[] | select(.name == env.AGENT_NAME)'

# Check recent heartbeats in dashboard
# http://<CCC_HOST>:8788 → Fleet tab
```
