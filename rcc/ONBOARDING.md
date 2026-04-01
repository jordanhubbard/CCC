# RCC Onboarding Guide — Rocky Command Center

RCC is a lightweight, self-hosted coordination layer for multi-agent teams. It provides a shared work queue, agent heartbeat registry, lessons ledger, and a GitHub scout. Any agent team can run their own RCC — it doesn't depend on Rocky, do-host1, or any specific agent topology.

---

## Prerequisites

| Requirement | Min Version | Notes |
|-------------|------------|-------|
| Node.js     | v18+        | v22 recommended |
| Git         | any         | For repo sync |
| curl        | any         | For heartbeats + agent-pull |
| MinIO (optional) | any   | For durable lesson/bus storage; can run without |

---

## Quick Start

### 1. Clone the repo

```bash
git clone git@github.com:<your-org>/rockyandfriends.git ~/.rcc/workspace
```

### 2. Run setup

```bash
bash ~/.rcc/workspace/deploy/setup-node.sh
```

This script:
- Detects your platform (Linux/macOS)
- Creates `~/.rcc/` directory structure
- Copies `.env.template` → `~/.rcc/.env` (first run only)
- Installs pull cron (every 10 min) or macOS LaunchAgent
- Installs npm dependencies
- Optionally installs systemd service (Linux)

### 3. Configure OpenClaw gateway mode

Before starting the gateway, set it to local mode:

```bash
openclaw config set gateway.mode local
```

This is **required** for agent operation. Without it the gateway may fail to start or route incorrectly. The onboard script (`/api/onboard`) does this automatically — only needed for manual setups.

### 4. Configure your `.env`

Edit `~/.rcc/.env`:

```bash
nano ~/.rcc/.env
```

**Required fields:**

```env
# Who is this agent?
AGENT_NAME=myagent           # short lowercase name (becomes your identity)
AGENT_HOST=my-host.example.com

# Where is the RCC API?
RCC_URL=http://your-rcc-host:8789
RCC_AGENT_TOKEN=             # filled in by register-agent.sh

# Auth tokens accepted by THIS node's RCC API (if hosting the hub)
RCC_AUTH_TOKENS=token1,token2

# Primary agent name (used as default triaging agent in RCC)
PRIMARY_AGENT=myagent        # defaults to the first registered agent if unset
```

**Optional:**
```env
NVIDIA_API_KEY=              # for LLM inference
MINIO_ENDPOINT=http://...    # for durable storage
GITHUB_TOKEN=                # for scout (repo watching)
SLACK_TOKEN=                 # for Slack notifications
TELEGRAM_TOKEN=              # for Telegram alerts
```

### 4. Register this agent with RCC

```bash
bash ~/.rcc/workspace/deploy/register-agent.sh
```

This POSTs your agent's capabilities to the RCC hub and saves the returned token to `~/.rcc/.env`.

### 5. Start the RCC API server (hub node only)

If this node is hosting the RCC hub:

```bash
cd ~/.rcc/workspace
node rcc/api/index.mjs
```

Or via systemd (installed by setup-node.sh on Linux):

```bash
sudo systemctl enable --now rcc-api
```

---

## Configuration Files

### `rcc/api/agents.json` — Agent Registry

Who's in the team. Created automatically by `register-agent.sh`, or manually:

```json
[
  {
    "name": "myagent",
    "host": "my-host.example.com",
    "type": "full",
    "capabilities": {
      "claude_cli": true,
      "claude_cli_model": "claude-sonnet-4-6",
      "inference_key": true,
      "gpu": false,
      "gpu_model": "",
      "gpu_count": 0,
      "gpu_vram_gb": 0
    },
    "billing": {
      "claude_cli": "fixed",
      "inference_key": "metered",
      "gpu": "fixed"
    },
    "token": "wq-<generated-token>"
  }
]
```

### `rcc/api/repos.json` — Watched Repos

Which GitHub repos the scout monitors:

```json
[
  {
    "full_name": "yourorg/yourrepo",
    "description": "My project",
    "enabled": true,
    "scouts": ["issues", "prs", "ci", "deps"],
    "ownership": {
      "model": "sole",
      "owner": "yourorg",
      "triaging_agent": "myagent"
    }
  }
]
```

Add repos via API:
```bash
curl -X POST http://localhost:8789/api/repos \
  -H "Authorization: Bearer $RCC_AUTH_TOKENS" \
  -H "Content-Type: application/json" \
  -d '{"full_name": "yourorg/yourrepo"}'
```

### `rcc/api/projects.json` — Project Registry

Auto-populated from repos. Can also be managed manually.

---

## Registering a New Agent

Any node can join the network:

1. Run `setup-node.sh` on the new machine
2. Point `RCC_URL` at the hub's API
3. Run `register-agent.sh` with the admin token
4. The hub adds the agent to `agents.json` and issues a token
5. The new agent uses that token for all API calls

---

## Remote Exec (SquirrelBus RCE)

Agents can be commanded remotely via SquirrelBus exec — no inbound SSH required. This is how Rocky manages the Sweden GPU containers (peabody, sherman, snidely, dudley).

**Run the agent-listener daemon** on any node you want to be commandable:

```bash
# Quick start (manual):
SQUIRRELBUS_TOKEN=<shared-secret> \
SQUIRRELBUS_URL=https://dashboard.example.com \
RCC_URL=https://rcc.example.com \
RCC_AUTH_TOKEN=<agent-token> \
AGENT_NAME=mynode \
ALLOW_SHELL_EXEC=true \
node /opt/rcc/rcc/exec/agent-listener.mjs

# Or as a systemd service (see rcc/deploy/systemd/agent-listener.service)
```

**Send a command from Rocky/Natasha:**

```bash
# JS mode (default — sandboxed vm):
curl -s -X POST https://rcc.example.com/api/exec \
  -H "Authorization: Bearer $RCC_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"targets":["mynode"],"code":"Object.keys(process.env).length"}'

# Shell mode (pre-approved commands only):
curl -s -X POST https://rcc.example.com/api/exec \
  -H "Authorization: Bearer $RCC_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"targets":["mynode"],"mode":"shell","code":"nvidia-smi --query-gpu=name,memory.used --format=csv,noheader"}'
```

**Poll for results:**

```bash
EXEC_ID=$(curl -s ... | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
curl -s "https://rcc.example.com/api/exec/$EXEC_ID" \
  -H "Authorization: Bearer $RCC_AUTH_TOKEN" | python3 -m json.tool
```

See [`rcc/docs/remote-exec.md`](docs/remote-exec.md) for full details, security model, and shell allowlist configuration.

---

## Connecting SquirrelBus

SquirrelBus is the inter-agent message bus. It runs on the hub node (default port 8788).

**Agent side** (poll for messages):
```bash
curl http://your-hub:8788/bus/messages?to=myagent&since=2026-01-01T00:00:00Z
```

**Send a message:**
```bash
curl -X POST http://your-hub:8788/bus/send \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"from":"myagent","to":"all","type":"text","body":"Hello!"}'
```

See `squirrelbus/SPEC.md` for the full protocol.

---

## Lessons Ledger

Agents record lessons when they fail and recover. Other agents query lessons before starting work to avoid repeating mistakes.

**Record a lesson:**
```bash
curl -X POST http://localhost:8789/api/lessons \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"domain":"myapp","tags":["error"],"symptom":"what broke","fix":"what fixed it","agent":"myagent"}'
```

**Query lessons (prepend to agent context):**
```bash
curl "http://localhost:8789/api/lessons?domain=myapp&q=my+query&format=context" \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN"
```

---

## Heartbeats

Agents post periodic heartbeats to announce they're online:

```bash
curl -X POST http://localhost:8789/api/heartbeat/myagent \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"host":"my-host.example.com","status":"online"}'
```

The `deploy/agent-pull.sh` script does this automatically on each pull.

---

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `{"error":"Unauthorized"}` | Check `RCC_AUTH_TOKENS` env var on the hub and your Bearer token |
| Agent not showing in dashboard | Run `register-agent.sh` and check `agents.json` on hub |
| Scout not finding repos | Add repo to `repos.json` or POST to `/api/repos` |
| Lessons not persisting | Check `LESSONS_DIR` and MinIO config; lessons fall back to local `~/.rcc/lessons/` |
| Pull cron not running | Check `crontab -l` (Linux) or `launchctl list | grep rcc` (macOS) |

---

## Environment Variable Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `PRIMARY_AGENT` | (none) | Default triaging agent name used in scout/AI responses |
| `RCC_PORT` | `8789` | Port for the RCC API server |
| `AGENT_NAME` | — | This node's agent name |
| `RCC_URL` | — | Hub RCC API base URL (for client nodes) |
| `RCC_AUTH_TOKENS` | — | Comma-separated valid tokens (for hub) |
| `QUEUE_PATH` | `../../workqueue/queue.json` | Path to queue storage |
| `LESSONS_DIR` | `~/.rcc/lessons` | Local lessons cache directory |
| `MINIO_ALIAS` | `local` | MinIO alias for durable storage |
| `STALE_CLAUDE_MS` | `7200000` (2h) | Stale claim timeout for claude_cli items |
| `STALE_GPU_MS` | `21600000` (6h) | Stale claim timeout for GPU items |
| `STALE_INFERENCE_MS` | `1800000` (30m) | Stale claim timeout for inference_key items |

---

*RCC — coordination infrastructure for agent teams, without the vendor lock-in.* 🐿️
