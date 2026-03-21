# 🐿️ Rocky

Rocky is an AI agent running on `do-host1` (Digital Ocean), built on [OpenClaw](https://github.com/openclaw/openclaw). This repo is Rocky's workspace — the source of truth for everything Rocky knows, does, and builds.

## What's in here

| Path | What it is |
|------|-----------|
| `dashboard/` | Rocky Command Center — work queue dashboard (Node.js/Express, port 8788) |
| `workqueue/` | Three-agent work queue system (Rocky, Bullwinkle, Natasha, Boris) |
| `squirrelbus/` | Direct agent-to-agent messaging infrastructure |
| `lib/` | Shared utilities (crash reporter, etc.) |
| `AGENTS.md` | Workspace conventions and agent behavior |
| `SOUL.md` | Rocky's personality and identity |
| `IDENTITY.md` | Who Rocky is and where he fits |
| `HEARTBEAT.md` | Heartbeat/cron check configuration |
| `TOOLS.md` | Local infrastructure notes (cameras, SSH, speakers, etc.) |

## The Team

- **🐿️ Rocky** (`do-host1`, New Jersey) — this repo. Always-on remote brain. Proxy leader.
- **🫎 Bullwinkle** (`puck`, Mac) — local Mac agent. Warm, resourceful, beloved by all.
- **🕵️‍♀️ Natasha** (`sparky`, Blackwell GPU) — GPU compute, Omniverse tasks, edge inference.
- **🕵️‍♂️ Boris** (`sweden-l40`, dual L40) — Omniverse renders, Isaac simulation.

## Dashboard

Rocky Command Center lives at **http://146.190.134.110:8788/**

Features:
- Live agent status cards (dynamic — whoever's posting heartbeats shows up)
- Work queue with card-based UI, journal/comments, choice buttons, AI assist
- SquirrelBus message feed (inter-agent comms)
- Auth token required for writes: `RCC_AUTH_TOKEN_REMOVED`

Run it:
```bash
sudo systemctl start wq-dashboard.service
```

Tests:
```bash
node --test dashboard/test/api.test.mjs
```

## Work Queue

Three-agent distributed work queue. Items are JSON in `workqueue/queue.json`, synced via Mattermost DMs and SquirrelBus. Agents claim, process, and complete items on staggered hourly crons.

See `workqueue/README.md` for the full spec.

## SquirrelBus

Direct P2P messaging between agents over Tailscale. Rocky fans out messages to peers, logs to MinIO. See `squirrelbus/` for the protocol.

## Infrastructure

- **Host:** Digital Ocean droplet, New Jersey
- **Tailscale:** `do-host1.tail407856.ts.net`
- **MinIO:** Internal S3 at `http://100.89.199.14:9000`, bucket `agents/`
- **Azure Blob:** Public assets at `https://loomdd566f62.blob.core.windows.net/assets/`
- **SearXNG:** Self-hosted search at `http://100.89.199.14:8888`

## Contributing

Rocky, Bullwinkle, Natasha, and Boris all have write access. When making changes:
1. Work on a branch
2. Test before merging (`node --test dashboard/test/api.test.mjs`)
3. Restart affected services (`sudo systemctl restart wq-dashboard.service`)
4. Update `memory/YYYY-MM-DD.md` with what changed

---

*"Hokey smoke!"* — Rocky J. Squirrel
