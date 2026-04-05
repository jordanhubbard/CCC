# CCC Hub

**Claw Command Center** — standalone hub server for multi-agent coordination.

Provides a work queue, agent registry, heartbeats, LLM routing, ClawBus messaging, and a web dashboard. Agents talk to this instead of maintaining local state copies.

## Quick Start

```bash
# 1. Clone or download
git clone https://github.com/jordanhubbard/rockyandfriends
cd rockyandfriends/rcc-hub

# 2. Install dependencies
npm install

# 3. Configure
node scripts/setup.mjs    # interactive wizard
# OR: cp .env.template .env && edit .env

# 4. Start
./start.sh
# or: npm start
```

The hub listens on `RCC_PORT` (default **8789**).

## One-liner install (Linux/Ubuntu)

```bash
curl -fsSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/rcc-hub/install-hub.sh | bash
```

## Configuration

See `.env.template` for all available options. Only three are required:

| Variable | Description |
|---|---|
| `RCC_PORT` | Port to listen on (default: 8789) |
| `CCC_AUTH_TOKENS` | Comma-separated agent Bearer tokens |
| `RCC_ADMIN_TOKEN` | Admin token (higher privilege) |

Everything else is optional and enables additional features (Milvus dedup, Slack, ClawBus, etc.).

## API

All endpoints require `Authorization: Bearer <token>`.

| Method | Path | Description |
|---|---|---|
| GET | `/health` | Health check (no auth) |
| GET | `/api/queue` | List work queue items |
| POST | `/api/queue` | Submit a new work item |
| PATCH | `/api/queue/:id` | Update item (claim, complete, etc.) |
| GET | `/api/agents` | List registered agents |
| POST | `/api/heartbeat/:agent` | Agent heartbeat |
| GET | `/api/exec` | List exec requests |
| POST | `/api/exec` | Submit exec to ClawBus |

Full API docs: [rcc/docs/](../rcc/docs/)

## Architecture

```
rcc-hub/
├── src/
│   ├── api/        # Main HTTP server (index.mjs)
│   ├── brain/      # LLM reasoning engine
│   ├── vector/     # Milvus embeddings + semantic dedup
│   ├── scout/      # GitHub repo scanning / pump
│   ├── llm/        # LLM provider registry
│   ├── lessons/    # Lessons-learned store
│   ├── ideation/   # Idea generation
│   └── issues/     # GitHub issue tracking
├── scripts/
│   └── setup.mjs   # Interactive setup wizard
├── .env.template   # All config options documented
└── start.sh        # Start with .env loaded
```

## Systemd (production)

```ini
[Unit]
Description=CCC Hub
After=network.target

[Service]
Type=simple
User=ubuntu
WorkingDirectory=/opt/rcc-hub
EnvironmentFile=/opt/rcc-hub/.env
ExecStart=/usr/bin/node src/api/index.mjs
Restart=always

[Install]
WantedBy=multi-user.target
```
