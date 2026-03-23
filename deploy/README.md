# Deploy — RCC Agent Node Setup

This directory contains everything needed to bootstrap a new agent node and keep it in sync with the repo.

## Quick Start — New Node

```bash
# 1. Clone the repo (or use the bootstrap script)
git clone git@github.com:jordanhubbard/rocky-and-friends.git ~/.rcc/workspace

# 2. Run setup (also installs tmux, checks for coding CLI, installs coding-agent skill)
bash ~/.rcc/workspace/deploy/setup-node.sh

# 3. Fill in your credentials
nano ~/.rcc/.env

# 4. Register with RCC
bash ~/.rcc/workspace/deploy/register-agent.sh

# 5. Test the pull
bash ~/.rcc/workspace/deploy/agent-pull.sh

# 6. Start your coding CLI turbocharger (do this once per machine)
tmux new-session -d -s claude-main
tmux send-keys -t claude-main 'claude --dangerously-skip-permissions' Enter
```

## The Coding CLI Turbocharger

OpenClaw coordinates. It doesn't do heavy coding in-process — that burns tokens and blocks everything else.

The turbocharger pattern: a coding CLI (Claude Code, Codex, OpenCode, Cursor, Pi) runs persistently in a tmux session. When a work item arrives with `preferred_executor: claude_cli`, the brain calls `workqueue/scripts/claude-worker.mjs`, which:
1. Finds the active coding CLI tmux session
2. Injects the task
3. Waits for completion
4. Returns output

Cost model: coding CLI = fixed monthly subscription, not per-token. OpenClaw's inference key stays for coordination only.

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

Required fields:
| Field | Description |
|-------|-------------|
| `AGENT_NAME` | Short name (rocky, bullwinkle, natasha, boris, or custom) |
| `AGENT_HOST` | Hostname for display |
| `RCC_URL` | URL of the RCC API server |
| `RCC_AGENT_TOKEN` | Bearer token (issued after registration) |
| `NVIDIA_API_KEY` | For LLM access |

Optional: MinIO creds, Azure Blob SAS, Slack/Mattermost/Telegram tokens.

## Supported Platforms

| Platform | Service Manager | Auto-pull |
|----------|----------------|-----------|
| Linux (systemd) | `rcc-agent.timer` + `rcc-agent.service` | ✅ |
| macOS | `com.rcc.agent.plist` (LaunchAgent) | ✅ |
| Other Linux | crontab | ✅ |

### Linux (systemd)
```bash
sudo cp deploy/systemd/rcc-agent.service /etc/systemd/system/
sudo cp deploy/systemd/rcc-agent.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now rcc-agent.timer
```

### macOS (Bullwinkle)
```bash
# setup-node.sh handles this automatically
# Or manually:
cp deploy/launchd/com.rcc.agent.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.rcc.agent.plist
```

## RCC API Server

The RCC API (`rcc/api/index.mjs`) runs on the hub node (Rocky, port 8789). It provides:

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
