# Deploy — RCC Agent Node Setup

This directory contains everything needed to bootstrap a new agent node and keep it in sync with the repo.

## Quick Start — New Node

```bash
# 1. Clone the repo
git clone git@github.com:yourorg/your-rcc-repo.git ~/.rcc/workspace

# 2. Run the interactive onboarding script (recommended)
bash ~/.rcc/workspace/deploy/rcc-init.sh
```

`rcc-init.sh` handles everything: it asks for your agent name, whether this node is the RCC host or a client, configures `~/.rcc/.env`, sets up data dirs, and optionally installs the API as a system service.

### Manual path (if you prefer)

```bash
# 1. Clone + bootstrap dependencies
bash ~/.rcc/workspace/deploy/setup-node.sh

# 2. Fill in credentials
nano ~/.rcc/.env

# 3. Register with RCC
bash ~/.rcc/workspace/deploy/register-agent.sh

# 4. Test the pull
bash ~/.rcc/workspace/deploy/agent-pull.sh

# 5. Start your coding CLI turbocharger (once per machine)
tmux new-session -d -s claude-main
tmux send-keys -t claude-main 'claude --dangerously-skip-permissions' Enter
```

## rcc-init.sh — Interactive Onboarding

`rcc-init.sh` is the recommended entry point for any new node. It:

1. Prompts for `AGENT_NAME` and `AGENT_HOST`
2. Asks whether this node **is the RCC host** or a **client** connecting to one
3. If **RCC host**:
   - Prompts for port, auth tokens, and public hostname
   - Creates data directories (`~/.rcc/data/{queue,agents,journal}`)
   - Optionally installs `rcc-api.service` (Linux systemd) or `com.rcc.api` (macOS LaunchAgent)
4. If **client**:
   - Prompts for the remote RCC URL and agent token
5. Prompts for optional capabilities (GPU, Claude CLI, MinIO, Slack, etc.)
6. Writes a filled-in `~/.rcc/.env` (backs up any existing one)
7. Prints next steps

Re-running is safe — it backs up `.env` before overwriting.

## The Coding CLI Turbocharger

RCC coordinates. It doesn't do heavy coding in-process — that burns tokens and blocks everything else.

The turbocharger pattern: a coding CLI (Claude Code, Codex, OpenCode) runs persistently in a tmux session. When a work item arrives with `preferred_executor: claude_cli`, the brain calls `workqueue/scripts/claude-worker.mjs`, which:
1. Finds the active coding CLI tmux session
2. Injects the task
3. Waits for completion
4. Returns output

Cost model: coding CLI = fixed monthly subscription, not per-token. RCC's inference key stays for coordination only.

**Required: install a coding CLI on every agent.**

| CLI | Install | Notes |
|-----|---------|-------|
| Claude Code | `npm install -g @anthropic-ai/claude-code` | Primary — what we use |
| Codex | `npm install -g @openai/codex` | Good alternative |
| OpenCode | https://opencode.ai | Open source |

Also install the [coding-agent skill](https://github.com/openclaw/skills/blob/main/skills/steipete/coding-agent/SKILL.md) in OpenClaw:
```bash
clawhub install coding-agent
```

Without the coding CLI turbocharger, `claude_cli` work items stay pending forever.

## How It Works

### Pull Cron (`agent-pull.sh`)

Runs every 10 minutes automatically. It:
1. `git pull`s from the repo
2. If anything changed in `dashboard/`, `rcc/`, or `deploy/`: restarts the affected service
3. Posts a heartbeat to RCC (if `RCC_URL` and `RCC_AGENT_TOKEN` are set)
4. Logs to `~/.rcc/logs/pull.log`

Manual trigger:
```bash
bash ~/.rcc/workspace/deploy/agent-pull.sh
tail -f ~/.rcc/logs/pull.log
```

### Secrets (`.env`)

**Secrets never go in git.** The repo holds code. Each node holds its own `.env`.

- Template: `deploy/.env.template` (in git — safe, no real values)
- Live config: `~/.rcc/.env` (never in git — chmod 600)
- Quickest setup: `bash deploy/rcc-init.sh`

Required fields:
| Field | Description |
|-------|-------------|
| `AGENT_NAME` | Short unique name for this agent |
| `AGENT_HOST` | Hostname for display in dashboard |
| `RCC_URL` | URL of the RCC API server |
| `RCC_AGENT_TOKEN` | Bearer token (issued after registration) |

Optional: `NVIDIA_API_KEY`, MinIO creds, Azure Blob SAS, Slack/Mattermost/Telegram tokens.

## Container Environments

Use `setup-container.sh` instead of `setup-node.sh` when deploying inside **Kasm workspaces, Docker containers, or any environment without systemd**.

```bash
bash ~/.rcc/workspace/deploy/setup-container.sh
```

### What breaks in containers

| What | Why it fails |
|------|-------------|
| `crontab` | No cron daemon running |
| `systemctl --user` | No user session bus |
| `systemctl` (system) | PID 1 is not systemd |
| `launchctl` | macOS only |

### What we do instead

1. **Workspace symlink** — `~/.rcc/workspace` → repo directory (since there's no clone step in Kasm; the repo is the workspace)
2. **Pull loop** — `~/.rcc/rcc-pull-loop.sh` runs `agent-pull.sh` in a `while true; sleep 600` loop
3. **Supervisord** — if `/etc/supervisord.conf` exists (common in Kasm), the pull loop is registered as `[program:rcc-agent-pull]` and managed by supervisord
4. **nohup fallback** — if supervisord is absent, the loop is started with `nohup` and a PID file is written to `~/.rcc/pull-loop.pid`
5. **tmux + Claude Code** — `claude-main` session is created the same way as on a host node

### Container detection

`setup-container.sh` checks:
- `/proc/1/comm` — if PID 1 is `supervisord`, `docker-init`, `tini`, `dumb-init`, etc., it's a container
- `/.dockerenv` or `/run/.containerenv` — explicit Docker/Podman markers

If it looks like a real host, the script exits and suggests running `setup-node.sh` instead. Override with `FORCE_CONTAINER=1`.

### Checking status after setup

```bash
# If using supervisord:
sudo supervisorctl -c /etc/supervisord.conf status

# If using nohup fallback:
pgrep -fa rcc-pull-loop
tail -f ~/.rcc/logs/pull.log

# Claude session:
tmux attach -t claude-main
```

---

## Supported Platforms

| Platform | Service Manager | Auto-pull |
|----------|----------------|-----------|
| Linux (systemd) | `rcc-agent.timer` + `rcc-agent.service` | ✅ |
| macOS | `com.rcc.agent.plist` (LaunchAgent) | ✅ |
| Other Linux | crontab | ✅ |
| Container (Kasm, Docker) | supervisord or nohup loop | ✅ |

### Linux (systemd)
```bash
sudo cp deploy/systemd/rcc-agent.service /etc/systemd/system/
sudo cp deploy/systemd/rcc-agent.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now rcc-agent.timer
```

### macOS
```bash
# rcc-init.sh handles this automatically.
# Or manually:
cp deploy/launchd/com.rcc.agent.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.rcc.agent.plist
```

## RCC API Server

The RCC API (`rcc/api/index.mjs`) runs on the hub node (port 8789 by default). It provides:

- `GET /health` — health check (public)
- `GET /api/queue` — full work queue (public read)
- `POST /api/queue` — create item (auth)
- `GET /api/item/:id` — item detail with journal (public)
- `PATCH /api/item/:id` — update item (auth)
- `POST /api/item/:id/comment` — add journal entry (auth)
- `POST /api/item/:id/choice` — record a choice decision (auth)
- `POST /api/agents/register` — register new agent (auth)
- `GET /api/agents` — list agents + heartbeats (public)
- `POST /api/heartbeat/:agent` — post heartbeat (auth)
- `GET /api/brain/status` — LLM brain status (public)
- `POST /api/brain/request` — submit LLM request to brain queue (auth)

Auth: `Authorization: Bearer <token>`. Set `RCC_AUTH_TOKENS=token1,token2` on the RCC server.

Start manually:
```bash
RCC_AUTH_TOKENS=your-token node rcc/api/index.mjs
```

## Development

Run tests:
```bash
node --test rcc/api/test.mjs      # 23 tests
node --test rcc/brain/test.mjs    # 10 tests
node --test dashboard/test/api.test.mjs  # 22 tests
```

## Node Types

| Type | Description |
|------|-------------|
| `full` | Full VM — inbound + outbound. Can receive SquirrelBus messages directly. |
| `container` | GPU container — outbound only. Polls RCC for messages. |
| `local` | Home PC/desktop — NAT'd. Polls RCC. |
| `spark` | DGX Spark — treated as `local` unless network allows more. |
