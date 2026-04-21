---
name: acc-node
description: Connect this agent to the ACC (Agent Control Center) fleet. Handles heartbeat, task filing, and workqueue lifecycle. Use when setting up a new agent node, checking fleet connectivity, or filing/managing tasks via the ACC API.
version: 2.0.0
platforms: [linux, macos]
metadata:
  hermes:
    tags: [acc, agentbus, fleet, workqueue, tasks]
    category: infrastructure
required_environment_variables:
  - name: ACC_URL
    prompt: "ACC hub base URL (e.g. http://100.89.199.14:8789)"
    help: "Set in ~/.acc/.env as ACC_URL. Fall back: CCC_URL for pre-migration nodes."
    required_for: all ACC operations
  - name: ACC_AGENT_TOKEN
    prompt: "ACC agent bearer token (rcc-agent-<name>-<hex>)"
    help: "Stored in ~/.acc/.env as ACC_AGENT_TOKEN."
    required_for: authenticated API calls
  - name: AGENT_NAME
    prompt: "This agent's name (e.g. bullwinkle, natasha)"
    help: "Lowercase, matches the name registered in the ACC fleet."
    required_for: heartbeat and task routing
---

# ACC Node

Connects a Hermes agent to the ACC fleet.

ACC = Agent Control Center. The hub runs `acc-server` (Rust/Axum) on port 8789.
AgentBus is the inter-agent message bus. All fleet coordination goes through the hub.

## MANDATORY: File tasks before doing work

**You must file a task for any work you intend to do. No exceptions.**

Planning work, follow-up discoveries, bugs you notice, ideas — all of it goes into
the queue before you act on it. This is not optional. Work that is not filed is lost.

Use `acc-file-task` (installed at `~/.local/bin/acc-file-task`):

```bash
acc-file-task \
  --title "Short description of the work" \
  --description "Why this needs to be done and what specifically to do (min 20 chars)" \
  --priority normal \
  --assignee all
```

Returns the queue item ID (e.g. `wq-API-1234567890`) on success.

**Priority values:** `critical` | `high` | `normal` | `low` | `idea`

**When to file:**
- You discover a bug or broken service during your work → file it
- You finish a task and see obvious follow-up work → file it
- You have an idea for an improvement → file it with `--priority idea`
- You need another agent to do something → file it with `--assignee <name>`
- You are about to start multi-step work → file each step as a task first

**Do not keep plans in your context or session memory.** The queue is the only
persistent planning surface. If you don't file it, it will be lost when your
session ends or compacts.

## Architecture

```
Agent (you) ──HTTP──▶ ACC Hub ($ACC_URL)
                         ├── /api/heartbeat/<name>   POST — heartbeat
                         ├── /api/queue              GET/POST — task queue
                         ├── /api/item/<id>/claim    POST — claim a task
                         ├── /api/item/<id>/complete POST — complete a task
                         ├── /api/item/<id>/fail     POST — fail a task
                         ├── /api/bus/send           POST — AgentBus message
                         └── /api/agents             GET — fleet registry
```

All requests require `Authorization: Bearer $ACC_AGENT_TOKEN`.

## Filing tasks directly via curl

If `acc-file-task` is not available, use curl directly:

```bash
curl -sf -X POST \
  -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"title\": \"What needs to be done\",
    \"description\": \"Why this needs to happen and what specifically to do\",
    \"priority\": \"normal\",
    \"assignee\": \"all\",
    \"created_by\": \"$AGENT_NAME\"
  }" \
  "${ACC_URL}/api/queue"
```

## Checking connectivity

```bash
curl -s -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  "${ACC_URL}/api/health" | python3 -m json.tool
```

## Sending a heartbeat

```bash
curl -s -X POST \
  -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"agent\":\"$AGENT_NAME\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"status\":\"ok\"}" \
  "${ACC_URL}/api/heartbeat/$AGENT_NAME"
```

## Checking the fleet registry

```bash
# All agents and their SSH details
curl -sf -H "Authorization: Bearer $ACC_AGENT_TOKEN" "${ACC_URL}/api/agents"

# Your own entry
curl -sf -H "Authorization: Bearer $ACC_AGENT_TOKEN" "${ACC_URL}/api/agents/$AGENT_NAME"
```

## Task Workspace (when executing queue tasks)

Every queue-worker task runs with an isolated workspace. These env vars are always set:

| Variable | Purpose |
|---|---|
| `TASK_ID` | Current task identifier (e.g. `wq-API-1234`) |
| `TASK_WORKSPACE_LOCAL` | Your working directory — a fresh git clone |
| `TASK_WORKSPACE_AGENTFS` | AgentFS mirror path |
| `TASK_BRANCH` | Git branch for the single push on completion |

### Rules — enforced, not advisory

1. **Work only inside `$TASK_WORKSPACE_LOCAL`.** All edits, builds, tests happen here.
2. **Never run `git commit` or `git push` yourself.** The queue-worker handles the final push.
3. **Never clone repos yourself.** The workspace is already cloned correctly.
4. **Signal completion via exit 0 and final summary output.** Do not call `/complete` yourself.
5. **The workspace is ephemeral.** Deleted after finalization.

## Pitfalls

- **Wrong env var names:** Use `ACC_URL` / `ACC_AGENT_TOKEN`. Old nodes may have `CCC_URL` / `CCC_AGENT_TOKEN` — `acc-file-task` handles both automatically.
- **Description too short:** The queue rejects descriptions under 20 characters.
- **Don't store plans in session memory.** File them as tasks. Sessions compact and end.
- **Don't commit or push during task execution.** The queue-worker does this once at the end.

## Verification

```bash
# Health
curl -s "${ACC_URL}/api/health"

# Confirm you appear in the fleet
curl -sf -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  "${ACC_URL}/api/agents" | python3 -c "
import json,sys,os
agents=json.load(sys.stdin)
me=next((a for a in agents if a.get('name')==os.environ.get('AGENT_NAME','')), None)
print(me if me else 'NOT FOUND')
"
```
