# Deploy — CCC Agent Node Setup

This directory contains everything needed to bootstrap a new agent node and keep it in sync with the repo.

## Quick Start — New Node

```bash
# 1. Clone the repo
git clone git@github.com:yourorg/your-ccc-repo.git ~/.ccc/workspace

# 2. Run the interactive onboarding script (recommended)
bash ~/.ccc/workspace/deploy/acc-init.sh
```

`acc-init.sh` handles everything: it asks for your agent name, whether this node is the CCC host or a client, configures `~/.ccc/.env`, sets up data dirs, and optionally installs the API as a system service.

### Manual path (if you prefer)

```bash
# 1. Clone + bootstrap dependencies
bash ~/.ccc/workspace/deploy/setup-node.sh

# 2. Fill in credentials
nano ~/.ccc/.env

# 3. Register with CCC
bash ~/.ccc/workspace/deploy/register-agent.sh

# 4. Test the pull
bash ~/.ccc/workspace/deploy/agent-pull.sh

# 5. Start your coding CLI turbocharger (once per machine)
tmux new-session -d -s claude-main
tmux send-keys -t claude-main 'claude --dangerously-skip-permissions' Enter
```

## acc-init.sh — Interactive Onboarding

`acc-init.sh` is the recommended entry point for any new node. It:

1. Prompts for `AGENT_NAME` and `AGENT_HOST`
2. Asks whether this node **is the CCC host** or a **client** connecting to one
3. If **CCC host**:
   - Prompts for port, auth tokens, and public hostname
   - Creates data directories (`~/.ccc/data/{queue,agents,journal}`)
   - Optionally installs `ccc-api.service` (Linux systemd) or `com.ccc.api` (macOS LaunchAgent)
4. If **client**:
   - Prompts for the remote CCC URL and agent token
5. Prompts for optional capabilities (GPU, Claude CLI, MinIO, Slack, etc.)
6. Writes a filled-in `~/.ccc/.env` (backs up any existing one)
7. Prints next steps

Re-running is safe — it backs up `.env` before overwriting.

## The Coding CLI Turbocharger

CCC coordinates. It doesn't do heavy coding in-process — that burns tokens and blocks everything else.

The turbocharger pattern: a coding CLI (Claude Code, Codex, OpenCode) runs persistently in a tmux session. When a work item arrives with `preferred_executor: claude_cli`, the brain calls `workqueue/scripts/claude-worker.mjs`, which:
1. Finds the active coding CLI tmux session
2. Injects the task
3. Waits for completion
4. Returns output

Cost model: coding CLI = fixed monthly subscription, not per-token. CCC's inference key stays for coordination only.

**Required: install a coding CLI on every agent.**

| CLI | Install | Notes |
|-----|---------|-------|
| Claude Code | `npm install -g @anthropic-ai/claude-code` | Primary — what we use |
| Codex | `npm install -g @openai/codex` | Good alternative |
| OpenCode | https://opencode.ai | Open source |

To install the CCC fleet skill into hermes-agent:
```bash
cp -r ~/.ccc/workspace/skills/acc-node/ ~/.hermes/skills/acc-node/
```

Without the coding CLI turbocharger, `claude_cli` work items stay pending forever.

## How It Works

### Pull Cron (`agent-pull.sh`)

Runs every 10 minutes automatically. It:
1. `git pull`s from the repo
2. If anything changed in `dashboard/`, `.acc/`, or `deploy/`: restarts the affected service
3. Posts a heartbeat to CCC (if `ACC_URL` and `ACC_AGENT_TOKEN` are set)
4. Logs to `~/.acc/logs/pull.log`

Manual trigger:
```bash
bash ~/.acc/workspace/deploy/agent-pull.sh
tail -f ~/.acc/logs/pull.log
```

### Secrets (`.env`)

**Secrets never go in git.** The repo holds code. Each node holds its own `.env`.

- Template: `deploy/.env.template` (in git — safe, no real values)
- Live config: `~/.acc/.env` (never in git — chmod 600)
- Quickest setup: `bash deploy/acc-init.sh`

Required fields:
| Field | Description |
|-------|-------------|
| `AGENT_NAME` | Short unique name for this agent |
| `AGENT_HOST` | Hostname for display in dashboard |
| `ACC_URL` | URL of the CCC API server |
| `ACC_AGENT_TOKEN` | Bearer token (issued after registration) |

Optional: `NVIDIA_API_KEY`, Azure Blob SAS, Slack/Telegram tokens, SMB credentials.

## Container Environments

Use `setup-container.sh` instead of `setup-node.sh` when deploying inside **Kasm workspaces, Docker containers, or any environment without systemd**.

```bash
bash ~/.ccc/workspace/deploy/setup-container.sh
```

### What breaks in containers

| What | Why it fails |
|------|-------------|
| `crontab` | No cron daemon running |
| `systemctl --user` | No user session bus |
| `systemctl` (system) | PID 1 is not systemd |
| `launchctl` | macOS only |

### What we do instead

1. **Workspace symlink** — `~/.ccc/workspace` → repo directory (since there's no clone step in Kasm; the repo is the workspace)
2. **Pull loop** — `~/.ccc/ccc-pull-loop.sh` runs `agent-pull.sh` in a `while true; sleep 600` loop
3. **Supervisord** — if `/etc/supervisord.conf` exists (common in Kasm), the pull loop is registered as `[program:ccc-agent-pull]` and managed by supervisord
4. **nohup fallback** — if supervisord is absent, the loop is started with `nohup` and a PID file is written to `~/.ccc/pull-loop.pid`
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
pgrep -fa ccc-pull-loop
tail -f ~/.ccc/logs/pull.log

# Claude session:
tmux attach -t claude-main
```

---

## Supported Platforms

| Platform | Service Manager | Auto-pull |
|----------|----------------|-----------|
| Linux (systemd) | `ccc-agent.timer` + `ccc-agent.service` | ✅ |
| macOS | `com.ccc.agent.plist` (LaunchAgent) | ✅ |
| Other Linux | crontab | ✅ |
| Container (Kasm, Docker) | supervisord or nohup loop | ✅ |

### Linux (systemd)
```bash
sudo cp deploy/systemd/ccc-agent.service /etc/systemd/system/
sudo cp deploy/systemd/ccc-agent.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now ccc-agent.timer
```

### macOS
```bash
# acc-init.sh handles this automatically.
# Or manually:
cp deploy/launchd/com.ccc.agent.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.ccc.agent.plist
```

## CCC API Server

The CCC API (.ccc/api/index.mjs`) runs on the hub node (port 8789 by default). It provides:

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

Auth: `Authorization: Bearer <token>`. Set `CCC_AUTH_TOKENS=token1,token2` on the CCC server.

Start manually:
```bash
CCC_AUTH_TOKENS=your-token node.ccc/api/index.mjs
```

## Development

Run tests:
```bash
node --test.ccc/api/test.mjs      # 23 tests
node --test.ccc/brain/test.mjs    # 10 tests
node --test dashboard/test/api.test.mjs  # 22 tests
```

## Node Types

| Type | Description |
|------|-------------|
| `full` | Full VM — inbound + outbound. Can receive AgentBus messages directly. |
| `container` | GPU container — outbound only. Polls CCC for messages. |
| `local` | Home PC/desktop — NAT'd. Polls CCC. |
| `spark` | DGX Spark — treated as `local` unless network allows more. |
