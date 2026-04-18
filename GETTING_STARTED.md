# Getting Started with CCC

> **Which path is yours?**
> - **Running your own CCC server** (hosting the coordination hub for your agents) → [Operator path](#operator-path)
>   - [Native install](#option-a-native-install) — deploy directly on a server you control
>   - [Docker install](#option-b-docker-install) — `docker compose up` on any Docker-capable host
> - **Adding an agent to an existing CCC deployment** → [Agent deployer path](#agent-deployer-path)
> - **Hacking on CCC itself** (modifying the codebase) → [Developer path](#developer-path)

---

## Operator Path

You want to run your own CCC instance on a server or VM you control.

### Prerequisites

- A Linux server or VM with a public IP (or accessible on your network)
- SSH access to that machine
- `git` and `make` installed locally

### Option A: Native Install

#### Step 1: Fork and clone

1. Fork this repo on GitHub
2. Clone your fork to your local machine:
   ```bash
   git clone https://github.com/jordanhubbard/ACC
   cd CCC
   ```

#### Step 2: Run the init wizard

```bash
make init
```

This interactive wizard will ask you:

- **Agent name** — a short name for your CCC host (e.g. `hub`, `homeserver`)
- **Role** — whether this machine IS the CCC host (yes, for the first one) or a client
- **CCC port** — defaults to `8789`
- **Auth tokens** — generate with `openssl rand -hex 32` (you'll share these with agent nodes)
- **Capabilities** — does this node have a GPU? A Claude Code session?
- **Optional integrations** — Slack, Telegram, MinIO (all skippable)
- **Channel selection** — pick which communication channels to enable

It writes `~/.ccc/.env` with your answers. That file is never committed to git.

#### Step 3: Start the services

Install and start the CCC API server (systemd, Linux):
```bash
sudo cp deploy/systemd/acc-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now acc-server
```

macOS:
```bash
cp deploy/launchd/com.ccc.agent.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.ccc.agent.plist
```

Open `http://your-server-ip:8789/health` — you should see `{"status":"ok"}`.

#### Step 4: Add agents

Once your CCC hub is running, add agents (other machines) using the [Agent deployer path](#agent-deployer-path) below. Each agent gets a token you generate:

```bash
openssl rand -hex 32
```

Share that token + your CCC URL with the new agent and have them run `make init`.

---

### Option B: Docker Install

The fastest path from "I have a server" to "CCC is running."

#### Prerequisites

- Docker and Docker Compose installed
- A clone of this repo (fork or direct)

#### Step 1: Clone and configure

```bash
git clone https://github.com/jordanhubbard/ACC
cd CCC
mkdir -p acc-data
cp deploy/.env.server.template acc-data/.env
nano acc-data/.env   # fill in ACC_AUTH_TOKENS, ACC_ADMIN_TOKEN, AGENT_NAME
```

Docker Compose reads config from `./acc-data/.env` (relative to the repo root).

#### Step 2: Start the stack

```bash
make docker-up
```

This brings up two containers:
- **acc-api** (port 8789) — the coordination API (Rust/Axum binary)
- **dashboard** (port 8788) — WASM web UI (nginx serving pre-built static files from `dist/`)

#### Step 3: Verify

```bash
curl http://localhost:8789/health
# → {"status":"ok"}
```

Open `http://your-server-ip:8788` in a browser to see the dashboard.

#### Other Docker commands

```bash
make docker-logs    # tail all container logs
make docker-down    # stop the stack
```

> **Pre-built images:** The CI publishes multi-arch images (amd64 + arm64) to
> `ghcr.io/jordanhubbard/acc:latest` on every push to main. See
> `.github/workflows/docker-publish.yml` for details. `make docker-up` uses the
> pre-built image by default (`CCC_IMAGE` env var overrides).

---

## Agent Deployer Path

Someone is already running a CCC hub and gave you a URL + token. You want to plug a new machine (GPU box, Mac mini, VPS, container) into their fleet.

### One-command bootstrap

```bash
curl -sSL https://raw.githubusercontent.com/jordanhubbard/ACC/main/deploy/bootstrap.sh | \
  bash -s -- \
    --ccc=https://ccc.your-operator-domain.example.com \
    --token=YOUR_BOOTSTRAP_TOKEN \
    --agent=my-new-agent
```

This will:
- Install hermes-agent (the standard agent runtime)
- Clone the CCC workspace to `~/.ccc/workspace`
- Write `~/.ccc/.env` with your hub credentials and any secrets from the bootstrap API
- Install hermes skills (acc-node, agent-skills, superpowers)
- Register hermes-gateway and ccc-bus-listener with supervisord
- Post a hardware fingerprint + heartbeat to the hub

Pass `--agent-token=<token>` to skip the bootstrap API call if you already have a known agent token.

### Manual bootstrap

If you prefer to set things up yourself:

1. Clone the repo:
   ```bash
   git clone https://github.com/jordanhubbard/ACC
   cd CCC
   ```

2. Run the init wizard in **client** mode:
   ```bash
   make init
   # Choose "2) Client" when asked for your role
   # Paste the CCC URL and your agent token when prompted
   ```

3. Register this agent with the hub:
   ```bash
   make register
   ```

4. Verify the connection:
   ```bash
   curl $CCC_URL/health
   ```

5. Start the agent runtime:
   ```bash
   hermes gateway
   ```

Your agent will now appear in the CCC dashboard.

---

## Developer Path

You want to modify CCC itself — add features, fix bugs, extend the protocol.

### Setup

```bash
git clone https://github.com/jordanhubbard/ACC
cd CCC
make init   # configure a local dev instance
```

The API server is a Rust binary (`acc-server`). The dashboard is Leptos WASM (pre-built dist committed at `dist/`).

```bash
make build   # build acc-server Rust binary
make test    # run Rust tests
```

### Project layout

| Path | What it is |
|------|-----------|
| `deploy/` | Setup scripts and systemd/launchd service units |
| `workqueue/` | Queue schema, executors, claude-worker.mjs |
| `agentbus/` | P2P message bus protocol + bus-listener |
| `skills/` | Shared agent skill configs |
| `scripts/` | Operator utilities (fleet monitor, Slack ingest, Qdrant tools) |
| `docs/` | Architecture and design docs |
| `onboarding/` | Per-agent onboarding docs |

---

## Configuration Reference

All config lives in `~/.ccc/.env`. The template at `deploy/.env.template` documents every variable.

Key variables:

| Variable | Purpose |
|----------|---------|
| `AGENT_NAME` | Short name shown in dashboard and logs |
| `AGENT_HOST` | Human-readable hostname |
| `CCC_URL` | URL of the CCC API hub |
| `CCC_AGENT_TOKEN` | Bearer token for this agent |
| `AGENT_HAS_GPU` | `true`/`false` — used for work routing |
| `AGENT_CLAUDE_CLI` | `true` if this node has a Claude Code tmux session |
| `TOKENHUB_URL` | TokenHub inference proxy URL (default `http://localhost:8090`) |
| `TOKENHUB_API_KEY` | Agent key for TokenHub |

For Docker deployments, config goes in `./acc-data/.env` relative to the repo root.

---

## Frequently Asked Questions

**Q: Do I need Docker?**
No. The native path (systemd or launchd) works on any Linux or macOS machine. Docker is an option for operators who prefer container management.

**Q: Can I use this without Slack or Telegram?**
Yes. Leave those tokens blank. The API and dashboard work independently of messaging channels.

**Q: What if my agents can't reach each other directly (firewalls, NAT)?**
Use the reverse SSH tunnel pattern. Each agent connects *out* to the CCC hub and forwards a local port. See the "Firewalled Agents" section in `README.md`.

**Q: My agent is ephemeral (container, spot instance). How do I handle that?**
Agents are expected to appear and disappear. The CCC hub tracks heartbeats and marks agents offline after a configurable timeout. Work items for an offline agent stay pending until a capable agent comes back. No manual intervention needed.

---

*See `README.md` for component specs. See `deploy/README.md` for service management.*
