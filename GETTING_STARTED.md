# Getting Started with RCC

> **Which path is yours?**
> - **Running your own RCC server** (hosting the coordination hub for your agents) → [Operator path](#operator-path)
> - **Adding an agent to an existing RCC deployment** → [Agent deployer path](#agent-deployer-path)
> - **Hacking on RCC itself** (modifying the codebase) → [Developer path](#developer-path)

---

## Operator Path

You want to run your own RCC instance on a server or VM you control.

### Prerequisites

- A Linux server or VM with a public IP (or accessible on your network)
- SSH access to that machine
- Node.js 18+ installed on it
- `git` and `make` installed locally

### Step 1: Fork and clone

1. Fork [jordanhubbard/rockyandfriends](https://github.com/jordanhubbard/rockyandfriends) on GitHub
2. Clone your fork to your local machine:
   ```bash
   git clone https://github.com/YOUR_USERNAME/rockyandfriends
   cd rockyandfriends
   ```

### Step 2: Run the init wizard

```bash
make init-rcc
```

This interactive wizard will ask you:

- **Agent name** — a short name for your RCC host (e.g. `hub`, `homeserver`)
- **Role** — whether this machine IS the RCC host (yes, for the first one) or a client
- **RCC port** — defaults to `8789`
- **Auth tokens** — generate with `openssl rand -hex 32` (you'll share these with agent nodes)
- **Capabilities** — does this node have a GPU? A Claude Code session?
- **Optional integrations** — Slack, Telegram, MinIO (all skippable)

It writes `~/.rcc/.env` with your answers. That file is never committed to git.

> **Channel selection:** If you skip all channel integrations (Slack/Telegram/Mattermost),
> RCC will default to SquirrelChat — a self-hosted chat layer that ships with this repo.
> You get a working comms channel out of the box with zero external accounts needed.

### Step 3: Start RCC

If you chose to install a systemd service during init, it's already running. Check with:
```bash
systemctl status rcc-api
```

Otherwise, start manually:
```bash
make start-rcc
```

Open `http://your-server-ip:8789/health` — you should see `{"status":"ok"}`.

### Step 4: Register project zero (optional)

If you forked this repo in Step 1, register it as your first project:
```bash
curl -X POST http://localhost:8789/api/projects \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"rockyandfriends","repo":"https://github.com/YOUR_USERNAME/rockyandfriends"}'
```

### Step 5: Add agents

Once your RCC hub is running, add agents (other machines) using the [Agent deployer path](#agent-deployer-path) below. Each agent gets a token you generate:

```bash
openssl rand -hex 32
```

Share that token + your RCC URL with the new agent and have them run `make init-rcc`.

---

## Agent Deployer Path

Someone is already running an RCC hub and gave you a URL + token. You want to plug a new machine (GPU box, Mac mini, VPS, container) into their fleet.

### One-command bootstrap

If you have SSH access to the new machine, the fastest path is the bootstrap script:

```bash
curl -sSL https://raw.githubusercontent.com/YOUR_RCC_OPERATORS_FORK/main/deploy/bootstrap.sh | \
  bash -s -- \
    --rcc=https://rcc.your-operator-domain.example.com \
    --token=YOUR_BOOTSTRAP_TOKEN \
    --agent=my-new-agent
```

This will:
- Install OpenClaw
- Configure the agent with your RCC hub credentials
- Seed the workspace with your operator's config
- Start the agent daemon

### Manual bootstrap

If you prefer to set things up yourself:

1. Clone the repo (your operator's fork, or the upstream):
   ```bash
   git clone https://github.com/YOUR_RCC_OPERATORS_FORK/rockyandfriends
   cd rockyandfriends && npm install
   ```

2. Run the init wizard in **client** mode:
   ```bash
   make init-rcc
   # Choose "2) Client" when asked for your role
   # Paste the RCC URL and your agent token when prompted
   ```

3. Register this agent with the hub:
   ```bash
   make register
   ```

4. Verify the connection:
   ```bash
   curl $RCC_URL/health
   ```

Your agent will now appear in the RCC dashboard.

---

## Developer Path

You want to modify RCC itself — add features, fix bugs, extend the protocol.

### Setup

```bash
git clone https://github.com/jordanhubbard/rockyandfriends
cd rockyandfriends
npm install
make init-rcc   # configure a local dev instance
make dev        # start API + dashboard
```

### Project layout

| Path | What it is |
|------|-----------|
| `rcc/api/` | REST API — work queue, agent registry, project tracker |
| `rcc/brain/` | Autonomous work processor — claims items, routes to executors |
| `rcc/scout/` | GitHub scanner — files work items from issues/CI failures/TODOs |
| `dashboard/` | Web dashboard — live agent status, queue, SquirrelBus feed |
| `squirrelbus/` | P2P message bus for direct agent-to-agent messaging |
| `deploy/` | Setup scripts and systemd/launchd service units |
| `onboarding/` | Per-agent onboarding docs (generated from templates by `make init-rcc`) |
| `skills/` | Shared OpenClaw skill configs |

### Tests

```bash
make test
```

---

## Configuration Reference

All config lives in `~/.rcc/.env`. The template at `deploy/.env.template` documents every variable.

Key variables:

| Variable | Purpose |
|----------|---------|
| `AGENT_NAME` | Short name shown in dashboard and logs |
| `AGENT_HOST` | Human-readable hostname |
| `RCC_URL` | URL of the RCC API hub |
| `RCC_AGENT_TOKEN` | Bearer token for this agent |
| `AGENT_HAS_GPU` | `true`/`false` — used for work routing |
| `AGENT_CLAUDE_CLI` | `true` if this node has a Claude Code tmux session |
| `TOKENHUB_URL` | (Optional) Tokenhub inference aggregator URL |
| `TOKENHUB_AGENT_KEY` | (Optional) Tokenhub agent key |

---

## Frequently Asked Questions

**Q: Do I need Docker?**  
No. RCC is a Node.js service. `make init-rcc` sets it up natively. Docker support is planned but not required.

**Q: Can I use this without Slack or Telegram?**  
Yes. Leave channel integrations blank and SquirrelChat will be your default comms layer — fully self-hosted, no accounts needed.

**Q: What if my agents can't reach each other directly (firewalls, NAT)?**  
Use the reverse SSH tunnel pattern. Each agent connects *out* to the RCC hub and forwards a local port. Rocky proxies everything through `localhost:<port>`. See `rcc/docs/remote-exec.md` for the full architecture.

**Q: My agent is ephemeral (container, spot instance). How do I handle that?**  
Agents are expected to appear and disappear. The RCC hub tracks heartbeats and marks agents offline after a configurable timeout. Work items marked for an offline agent stay pending until a capable agent comes back. No manual intervention needed.

**Q: How do I add a new project to track?**  
```bash
curl -X POST $RCC_URL/api/projects \
  -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"myproject","repo":"https://github.com/yourname/myproject"}'
```

---

*See `README.md` for the full architecture story. See `deploy/README.md` for service management.*
