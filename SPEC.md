# CCC System Specification

**Version:** Updated 2026-04-15 (post-refactor)  
**Scope:** Complete description of what CCC does, based on codebase audit and live inspection of running instances.

This document describes the system as designed — the components, their roles, and how they interact — without enumerating specific hosts or fleet segments, which are transient deployment details.

---

## 1. What CCC Is

CCC (Claw Command Center, previously "RCC") is a personally operated, distributed AI agent coordination system. It connects a fleet of heterogeneous Linux machines and lets them collaborate on long-running tasks through a shared work queue, a message bus, and a central API server.

It is not a product, framework, or general-purpose platform. It is bespoke infrastructure built around a specific operational reality: outbound-only containers, NAT'd home machines, and a mix of Claude CLI agents and local GPU inference nodes that need to coordinate without always being directly reachable from the hub.

---

## 2. Fleet Roles

CCC supports two node roles. A fleet of any size can run one hub and any number of agent nodes.

### Hub Node

Runs the central services: `ccc-server`, `minio`, `qdrant`, `tokenhub`, and `agentfs-sync`. The hub is the single source of truth for the work queue, message bus, agent registry, and secrets store. It is the only node that must be reachable by other agents.

Required services on the hub:
- `ccc-server` — Rust/Axum API server (port 8789)
- `minio` — S3-compatible object storage (port 9000, typically localhost-only)
- `qdrant` — Vector database (port 6333/6334)
- `tokenhub` — LLM routing proxy (port 8090)
- `agentfs-sync` — Workspace ↔ MinIO sync daemon
- `dashboard-server` — Dashboard UI (port 8790)
- `ccc-agent listen` — ClawBus exec listener

### Agent Node

Runs `hermes-agent` as the primary runtime. Connects to the hub via HTTP. Polls the hub for work, sends heartbeats, and optionally runs local inference.

Typical services on an agent node:
- `hermes gateway` — Channel gateway (Slack, Telegram); supervisord-managed
- `ccc-agent listen` — ClawBus exec listener
- `qdrant` — Local vector DB (optional, for local memory)
- `tokenhub` — Local LLM router (optional)
- `ollama` — Local model serving (GPU nodes only)
- `whisper-server` — Speech-to-text (optional)

An agent node requires only outbound HTTP access to the hub. It does not need to be publicly reachable.

---

## 3. ccc-server (Hub API)

**Binary:** `ccc-server` (Rust, Axum), installed at `/usr/local/bin/ccc-server`  
**Port:** 8789  
**Config file:** `~/.ccc/ccc.json`  
**Auth:** Bearer token required on all routes  
**CORS:** Open (`Any`) by default; configurable via `CCC_CORS_ORIGINS`

### Primary Storage

State is stored as JSON files, loaded into memory at startup and periodically flushed to disk. SQLite is an optional migration target: if `db_path` is configured, the server opens the DB and migrates JSON data into it, then **immediately drops the connection** — the comment in `main.rs` explicitly states "conn is not yet stored in AppState — for now, migration only." At present the server operates purely from JSON files.

**Data directory:** `~/.ccc/data/`

| File | Purpose |
|---|---|
| `queue.json` | Work queue — all pending, in-progress, and completed items |
| `agents.json` | Agent registry — names, capabilities, last heartbeat |
| `secrets.json` | Key-value secret store |
| `bus.jsonl` | ClawBus message log (append-only JSONL) |
| `exec.jsonl` | Remote execution log |
| `brain-state.json` | Brain/LLM queue state |
| `lessons.jsonl` | Shared lessons/knowledge entries |
| `repos.json` | Repository tracking data |

**Sizes observed in the running hub:** queue.json ~2.5 MB (substantial active history), bus.jsonl ~118 KB / 190 messages.

### Supplemental Storage

- **Auth DB:** `~/.ccc/auth.db` (SQLite) — user credentials, token hashes
- **MinIO/S3:** Object storage, bucket `agents` — for file storage via `/api/fs`
- **Qdrant:** Local instance — for vector search via `/api/memory`

### API Routes

All routes confirmed present in source (Rust server lives in ClawFS at `/home/jkh/clawfs/repos/CCC`, accessed via `mc`):

**Workqueue** (`queue.rs`):

| Method | Path | Description |
|---|---|---|
| GET | `/api/queue` | Return all queue items + completed |
| POST | `/api/queue` | Create new queue item |
| GET | `/api/queue/stale` | Items in-progress past their stale threshold |
| GET | `/api/queue/claimed` | Items currently claimed by an agent |
| GET | `/api/item/:id` | Get single item |
| PATCH | `/api/item/:id` | Update item fields |
| DELETE | `/api/item/:id` | Delete item |
| POST | `/api/item/:id/claim` | Mark item claimed by caller |
| POST | `/api/item/:id/complete` | Mark item completed with result |
| POST | `/api/item/:id/fail` | Mark item failed |
| POST | `/api/item/:id/keepalive` | Extend claim deadline |
| POST | `/api/item/:id/stale-reset` | Clear stale claim (reset claimedBy/At) |

Stale thresholds vary by executor type: `claude_cli` = 45 min, `gpu` = 120 min, `llm_server` = 60 min, default = 30 min.

**Agents** (`agents.rs`):

| Method | Path | Description |
|---|---|---|
| GET | `/api/agents` | List all agents (includes computed `online` field) |
| POST | `/api/agents` | Create agent record |
| POST | `/api/agents/register` | Register agent with capabilities |
| GET | `/api/agents/:name` | Get single agent |
| POST/PATCH | `/api/agents/:name` | Upsert / patch agent record |
| DELETE | `/api/agents/:name` | Remove agent |
| POST | `/api/agents/:name/heartbeat` | Post heartbeat for agent |
| GET | `/api/agents/:name/health` | Get agent telemetry from last heartbeat |
| POST | `/api/heartbeat/:agent` | Alias for agent heartbeat |
| GET | `/api/heartbeats` | List heartbeat status for all agents |

**Online detection:** Agent is considered online if `lastSeen` is within **300 seconds** (5 minutes), confirmed in `agents.rs:28`.

**ClawBus** (`bus.rs`):

| Method | Path | Description |
|---|---|---|
| GET | `/api/bus/stream` | SSE stream — real-time message delivery; agents subscribe via `bus-listener.sh` |
| POST | `/api/bus/send` | Send message to bus; used by `fleet-sync.sh` to broadcast `rcc.update` |
| GET | `/api/bus/messages` | Query messages (params: `limit`, `subject`, `type`, `thread_id`, `to`, `from`) |
| GET | `/api/bus/presence` | Agent presence (online/offline status) |

All four routes are duplicated under `/bus/*` for reverse-proxy compatibility. ClawBus is **not a separate process** — it runs inside `ccc-server` on port 8789. The viewer at port 8790 is the dashboard-server proxy.

**Secrets** (`secrets.rs`): GET/POST/PUT/DELETE on `/api/secrets` and `/api/secrets/:key`

**Other modules** (routes confirmed in source; internal implementation not fully audited):

| Module | Route prefix | Purpose |
|---|---|---|
| `health.rs` | `/api/health` | Server uptime, queue depth |
| `projects.rs` | `/api/projects` | Project management |
| `brain.rs` | `/api/brain` | LLM request queue |
| `services.rs` | `/api/services` | Service discovery |
| `lessons.rs` | `/api/lessons` | Shared knowledge ledger |
| `exec.rs` | `/api/exec` | Remote execution results |
| `geek.rs` | `/api/geek` | Debug/diagnostics |
| `ui.rs` | (static) | Optional SPA fallback (`DASHBOARD_DIST`) |
| `memory.rs` | `/api/memory` | Agent memory (Qdrant-backed) |
| `issues.rs` | `/api/issues` | Issue tracking |
| `fs.rs` | `/api/fs` | File/S3 access |
| `supervisor.rs` | `/api/supervisor` | Optional process supervisor control |
| `conversations.rs` | `/api/conversations` | Conversation history |
| `setup.rs` | `/api/setup` | Onboarding endpoints |
| `providers.rs` | `/api/providers` | Model provider configuration |
| `acp.rs` | `/api/acp` | (purpose not fully audited) |
| `models.rs` | `/api/models` | Model registry |
| `auth.rs` | `/api/auth` | Authentication |
| `metrics.rs` | `/api/metrics` | Metrics collection |

### Optional Process Supervisor

ccc-server can optionally manage a `tokenhub` child process with restart-on-failure semantics. Disabled by default; enabled via config.

---

## 4. Work Queue

The work queue is the primary coordination mechanism between agents. The hub holds the authoritative state; agents poll and claim items.

### Item Schema

```json
{
  "id": "wq-YYYYMMDD-NNN",
  "itemVersion": 1,
  "created": "ISO-8601",
  "source": "<agent-name>|<human>",
  "assignee": "<agent-name>|all|<human>",
  "priority": "urgent|high|normal|low|idea",
  "status": "pending|in-progress|completed|failed|blocked|deferred",
  "title": "...",
  "description": "...",
  "notes": "...",
  "tags": ["maintenance", "infrastructure", ...],
  "preferred_executor": "claude_cli|claude_sdk|codex_cli|...",
  "required_executors": ["claude_sdk", "codex_vllm"],
  "claimedBy": null | "<agent-name>",
  "claimedAt": null | "ISO-8601",
  "attempts": 0,
  "maxAttempts": 3,
  "completedAt": null | "ISO-8601",
  "result": null | "summary text"
}
```

`preferred_executor` is a soft hint — the dispatcher uses it if the agent supports it. `required_executors` is a hard filter — an agent may only claim the item if its capability manifest includes at least one of the listed types. Items with unsatisfied `required_executors` remain pending until a capable agent comes online.

### Executor Types

| Type | Backend | Auth |
|---|---|---|
| `claude_cli` | `claude --print` subprocess | `~/.claude/credentials` or `ANTHROPIC_API_KEY` |
| `claude_sdk` | `@anthropic-ai/claude-code` `query()` | Same `~/.claude/credentials` |
| `codex_cli` | `codex --approval-mode full-auto` | `OPENAI_API_KEY` |
| `codex_vllm` | `codex` → local vLLM endpoint | None (local GPU) |
| `cursor_cli` | `cursor --headless` (experimental) | `CURSOR_SESSION_TOKEN` |
| `opencode` | `opencode run` → ollama / vLLM | `OPENAI_API_KEY` or `none` |
| `inference_key` | Direct API call, no coding agent | `NVIDIA_API_KEY` / cloud key |

### Claim and Stale Logic

- An agent claims an item by writing its name to `claimedBy` and a timestamp to `claimedAt`
- If `claimedAt` is older than the stale threshold for the item's executor type, any other agent can clear the claim via `stale-reset`
- Stale thresholds: `claude_cli` = 45 min, `gpu` = 120 min, `llm_server` = 60 min, default = 30 min

### Priority Levels

| Priority | Meaning |
|---|---|
| `urgent` | Drop everything |
| `high` | Process before normal items |
| `normal` | Standard work |
| `low` | Process when idle |
| `idea` | Proposal; requires peer votes to become active |

---

## 5. Executor Routing

`workqueue/executors/dispatch.mjs` is the task executor router. It is invoked by `workqueue/scripts/run-coding-agent.sh` (which is now a thin wrapper) and directly by agent runtimes that handle coding tasks.

### Routing Priority

1. `item.required_executors` — hard filter; if set, executor must be in this list
2. `item.preferred_executor` — soft hint used within the allowed set
3. `agentConfig.defaultExecutor` — agent-level default
4. `claude_cli` — global fallback

### Executor Files (`workqueue/executors/`)

| File | Executor |
|---|---|
| `dispatch.mjs` | Router — entry point |
| `claude-sdk.mjs` | `claude_sdk` — structured output, token cost, session IDs |
| `claude-cli.mjs` | `claude_cli` — subprocess; throws `ThrottleError` on rate-limit |
| `codex.mjs` | `codex_cli` / `codex_vllm` — parameterised by `baseUrl` |
| `opencode.mjs` | `opencode` — auto-detects provider (explicit → ollama → vLLM) |
| `cursor.mjs` | `cursor_cli` — opt-in, experimental |

### Stale Thresholds by Executor

| Executor | Stale after |
|---|---|
| `claude_cli`, `claude_sdk` | 45 min |
| `codex_vllm`, `gpu` | 120 min |
| `opencode`, `codex_cli` | 60 min |
| default | 30 min |

---

## 6. ClawBus (Message Bus)

ClawBus is the inter-agent message bus. The hub maintains an in-memory broadcast channel (Tokio `broadcast::channel`, capacity 256) and appends all messages to `bus.jsonl`.

### Message Format

```json
{
  "id": "<uuid>",
  "from": "<agent-name>|<human>",
  "to": "<agent-name>|all",
  "ts": "ISO-8601",
  "seq": 42,
  "type": "text|heartbeat|queue_sync|rcc.exec|rcc.quench|...",
  "mime": "text/plain|...",
  "enc": "none|base64",
  "body": "...",
  "ref": null | "<message-id>",
  "subject": null | "<channel-name>",
  "thread_id": null | "...",
  "ttl": 604800
}
```

### Delivery

- **SSE stream** (`/api/bus/stream`): Agents connect and receive messages in real time. This is the primary delivery mechanism for `rcc.exec` remote execution commands.
- **Poll** (`/api/bus/messages`): Dashboard and agents can query message history with filters: subject, type, thread_id, from/to DM pairs, since-timestamp. Default limit 500, max 2000.
- **Append log** (`bus.jsonl`): All messages persisted to disk.

### Known Message Types

| Type | Body | Purpose |
|---|---|---|
| `text` | Plain string | Human-readable message |
| `heartbeat` | `{"status":"online"}` | Agent presence signal |
| `queue_sync` | Queue JSON | Workqueue state synchronization |
| `rcc.exec` | `{execId, code, target, mode, sig}` | Remote code execution request |
| `rcc.quench` | `{minutes, reason}` | Signal agent to pause work |

---

## 7. Remote Execution (ccc-agent listen)

The exec-listener daemon runs on each node and enables remote code execution dispatched from the hub.

**How it works:**
1. `ccc-agent listen` subscribes to the hub's SSE stream at `/api/bus/stream`
2. On receiving a message with `type: "rcc.exec"`, it parses: `{execId, code, target, mode, sig}`
3. Executes the code via `/bin/sh`
4. Posts result to `/api/exec/<execId>/result`: `{"result": "...", "exitCode": 0}`

All results are logged to `exec.jsonl` on the hub. Observed size on the running hub: ~23 KB.

On hub nodes this typically runs as a systemd service (`ccc-exec-listen.service`). On agent nodes it may run via cron or be started by the agent runtime.

---

## 8. ccc-agent CLI

The `ccc-agent` binary is a small Rust CLI installed at `~/.ccc/bin/ccc-agent`. It provides utilities used by shell scripts and the migration framework, plus the exec-listener daemon.

| Subcommand | Description |
|---|---|
| `listen` | Long-running exec-listener daemon |
| `migrate is-applied <name>` | Check if migration has been applied |
| `migrate record <name> ok\|failed` | Record migration result in `~/.ccc/migrations.json` |
| `migrate list <dir>` | Show applied/pending migrations |
| `agent init <path> --name=X --host=X --version=X` | Write `agent.json` at first onboard |
| `agent upgrade <path> --version=X` | Update version fields in `agent.json` |
| `json get <path> [fallback]` | Extract scalar from JSON stdin |
| `json lines <path>` | Print array elements one per line |
| `json pairs <path>` | Print object as `key=value` lines |
| `json env-merge <path> <file>` | Merge a JSON secrets object into a `.env` file |

---

## 9. Agent Runtime: hermes-agent

Hermes is the primary AI agent runtime on all nodes. It replaced OpenClaw as the standard runtime. Agents are identified by `AGENT_NAME` in their environment.

**Binary:** Python, installed as `hermes-agent[slack]` (includes `slack-bolt` and `slack-sdk`) via `pipx` or a venv. The `[slack]` extra is mandatory for Slack channel connectivity.

**Per-node data at `~/.hermes/`:**

| File/Dir | Purpose |
|---|---|
| `config.yaml` | CCC env vars, channel tokens, LLM provider config — written by `bootstrap.sh` |
| `state.db` | SQLite: sessions, messages, conversation history |
| `memory_store.db` | SQLite: structured agent memory |
| `memories/` | Individual memory files |
| `sessions/` | Session data |
| `skills/` | Installed hermes skills |
| `SOUL.md` | Agent personality/identity |
| `MEMORY.md` | Cross-session factual memory (typed-network schema) |
| `USER.md` | Operator profile |
| `channel_directory.json` | Active channel connections |
| `gateway.pid` / `gateway_state.json` | Gateway process state |

**`config.yaml` contents (written by bootstrap.sh):**
```yaml
env:
  CCC_URL: "<hub url>"
  CCC_AGENT_TOKEN: "<agent bearer token>"
  AGENT_NAME: "<name>"
  SLACK_BOT_TOKEN: "<xoxb-...>"   # if Slack tokens were obtained from CCC secrets
  SLACK_APP_TOKEN: "<xapp-...>"
```

**Gateway process:** `hermes gateway` manages all messaging channel connections (Slack, Telegram, etc.) and must run as a long-lived process. `bootstrap.sh` registers it with supervisord automatically (writes to `/etc/supervisor/conf.d/hermes-gateway.conf` or appends to the monolithic conf). Log: `~/.ccc/logs/hermes-gateway.log`.

**Size observed in practice:** `state.db` grows to 10–111 MB on active nodes depending on session history volume.

**LLM routing:** Hermes supports multiple provider backends. In the observed deployments, model requests are routed through NVIDIA's inference API for Claude models, and through local TokenHub for routing across backends. Claude Code CLI (when running alongside hermes) also routes through the same NVIDIA endpoint via `ANTHROPIC_BASE_URL`.

**Skills installed by bootstrap.sh:**
- `ccc-node` — fleet connectivity (heartbeat, workqueue, ClawBus)
- 20+ engineering workflow skills from `addyosmani/agent-skills` (cloned to `~/.ccc/agent-skills`, pulled fresh on each bootstrap run) — domain implementation: spec, planning, TDD, frontend, API, security, CI/CD, shipping
- 13 orchestration skills from `obra/superpowers` (cloned to `~/.ccc/superpowers`) — coordination discipline: parallel agent dispatch, git worktrees, systematic debugging, two-stage review, verification gates

---

## 10. TokenHub (LLM Router)

TokenHub is an LLM routing proxy that aggregates multiple inference backends and exposes a unified OpenAI-compatible API at port 8090.

Backends routed include cloud inference APIs (NVIDIA, others) and local model servers (Ollama, vLLM). Agents configure `TOKENHUB_URL` to point to a local or hub-hosted TokenHub instance.

On hub nodes TokenHub runs as a standalone systemd service. On GPU agent nodes it may run inside a container.

---

## 11. agentfs-sync

`agentfs-sync` is a daemon that mirrors the workspace to/from MinIO (S3) at the hub. It provides durable shared storage so agents can access workspace state without direct git access.

Runs as a systemd service (`agentfs-sync.service`) on the hub. Not required on agent nodes that have direct git access.

---

## 12. dashboard-server

A Rust binary that provides the operator dashboard UI. Observed startup log:

```
RCC Dashboard v2 starting port=8790 rcc_url=http://localhost:8789 sc_url=http://localhost:8793 operator=jkh
```

It proxies requests to `ccc-server` (8789) and optionally to SquirrelChat (8793, a separate chat service that has its own codebase not in this repo).

**Alternative frontend:** `ccc-server` can serve a WASM/Leptos SPA directly as a fallback when `DASHBOARD_DIST` is set. The SPA source lives in ClawFS alongside the Rust server source. As of audit, the `dashboard-server` binary approach is what's running.

---

## 13. Deploy and Maintenance

### Operator Control (any machine)

Any machine with internet access and `~/.ccc/.env` credentials can control and deploy changes to the fleet — macOS, Linux, or WSL2. No special access beyond the hub token is required.

**One-time setup:**
```bash
make deps    # installs mc (MinIO client), gh, jq for your platform
make env     # creates ~/.ccc/.env from template; fill in CCC_URL + CCC_AGENT_TOKEN
```

**Deploying changes:**
```bash
git push && bash deploy/fleet-sync.sh
# or equivalently:
make sync
```

`fleet-sync.sh` / `make sync`:
1. Mirrors `~/.ccc/workspace/` → MinIO (`agents/shared/workspace/`) via `mc mirror` — workspace is in agentfs and accessible to agents via `mc` without git
2. Checks `/bus/presence` to show which agents are currently online
3. Broadcasts `rcc.update` to ClawBus — agents running `bus-listener.sh` run `agent-pull.sh` within seconds

Agents without `bus-listener.sh` running fall back to the 10-minute pull timer.

**`make deps` platform support:**

| Platform | mc install method | gh/jq install |
|---|---|---|
| macOS | `brew install minio/stable/mc` | `brew install gh jq` |
| Ubuntu/Debian/WSL2 | Binary from `dl.min.io` | apt + GitHub CLI repo |
| RHEL/Rocky/Fedora | Binary from `dl.min.io` | dnf + GitHub CLI repo |

### Continuous Pull (agent-pull.sh)

Runs every 10 minutes via cron on all nodes. Steps:
1. `git pull --ff-only origin <branch>` — update workspace repo
2. Detect changed files; restart affected services if deployment-relevant files changed
3. `npm install` if `package.json` changed
4. Sync secrets from hub via `secrets-sync.sh`
5. Post heartbeat to hub with current git revision and hardware info

### ClawBus Listener (bus-listener.sh)

Long-lived daemon registered by `bootstrap.sh` as `ccc-bus-listener` under supervisord. Subscribes to `GET /bus/stream` (SSE). On `rcc.update` runs `agent-pull.sh` immediately — eliminates the 10-minute polling gap for triggered deploys. Log: `~/.ccc/logs/bus-listener.log`.

### Memory Commit (memory-git-commit.sh)

Runs daily at midnight via cron. Stages `MEMORY.md` and `memory/` directory and creates a git commit if there are changes. Provides git-backed time-travel history of agent memory that propagates to all nodes via `agent-pull.sh`.

### Migrations (deploy/migrations/)

14 numbered shell scripts (0001–0014), applied via `bash deploy/run-migrations.sh`. State tracked in `~/.ccc/migrations.json`. Each script is idempotent and safe to re-run.

| Migration | Description |
|---|---|
| 0001 | Remove rcc-* renamed services |
| 0002 | Tear down openclaw and legacy services |
| 0003 | Install ccc-pull cron service |
| 0004 | Install Node.js services (superseded) |
| 0005 | Install hub API services |
| 0006 | Remove Node.js services |
| 0007 | Switch to Rust ccc-server, remove old dashboard |
| 0008 | Rebuild ccc-server with auth, install auth.db |
| 0009 | Install Consul service discovery |
| 0010 | Configure Consul DNS |
| 0011 | Build ccc-agent binary |
| 0012 | Install ccc-exec-listen service |
| 0013 | Remove ClawFS FUSE mount (use S3 gateway only) |
| 0014 | Stop and disable Consul; remove stale env vars (`RCC_*`, `CONSUL_*`, legacy paths); fix consul DNS URLs to use `localhost` |

**Consul:** Installed by migrations 0009/0010, torn down by migration 0014. Not in use — all services use `localhost` directly.

### Bootstrap (bootstrap.sh)

One-command onboarding for new agent nodes:
1. Install `hermes-agent[slack]` (pipx preferred, pip3 fallback); on existing installs injects `slack-bolt`/`slack-sdk` if missing
2. Clone CCC workspace to `~/.ccc/workspace`
3. Call bootstrap API to consume a one-time token and receive an agent token + secrets bundle
4. Write `~/.ccc/.env` with all credentials (including any Slack/Telegram channel tokens)
5. Install agentfs-sync if available from MinIO
6. Configure vLLM if a GPU is detected (`nvidia-smi`)
7. Write `~/.hermes/config.yaml` with `CCC_URL`, `CCC_AGENT_TOKEN`, `AGENT_NAME`, and channel tokens
8. Register `hermes-gateway` with supervisord (conf.d preferred; falls back to monolithic conf or warns)
9. Register `ccc-bus-listener` with supervisord (runs `bus-listener.sh`; nohup fallback if no supervisord)
10. Clone `addyosmani/agent-skills` to `~/.ccc/agent-skills` (or pull to update); install all 20+ skills into `~/.hermes/skills/`
11. Clone `obra/superpowers` to `~/.ccc/superpowers` (or pull to update); install 13 orchestration skills into `~/.hermes/skills/`
12. Install `ccc-node` skill into `~/.hermes/skills/`
13. Collect hardware fingerprint, post heartbeat and capabilities to hub
14. Write `~/.ccc/agent.json` (onboarding signature with version and timestamp)

### setup-node.sh

Idempotent node setup script (as opposed to first-time bootstrap). Installs:
- Pull cron (every 10 minutes)
- Ops crons (memory-commit daily at midnight)
- Hermes agent runtime (same install logic as bootstrap)
- ccc-node skill into hermes
- Seeded `MEMORY.md` with typed-network fleet context (if not already present)
- vLLM setup if GPU detected

---

## 14. Networking

### Agent Connectivity Model

Agents only need outbound HTTP to the hub. The hub does not initiate connections to agents — all work is delivered via the ClawBus SSE stream that agents subscribe to, or via the exec-listener which also polls inbound.

**Supported access paths:**
- Direct LAN or private IP
- Tailscale mesh (all nodes joined to the same tailnet)
- Public IP / hostname (hub only, for nodes without Tailscale)
- SSH tunnel (outbound-only containers with no inbound reachability)

### Standard Port Assignments

**Hub node:**

| Port | Service | Notes |
|---|---|---|
| 8789 | ccc-server | Main CCC API (all agents connect here) |
| 8790 | dashboard-server | Operator dashboard |
| 9000 | MinIO | Object storage (typically localhost-only) |
| 9100 | ClawFS S3 gateway | JuiceFS S3 API — all agents access ClawFS here via `mc` |
| 8090 | tokenhub | LLM router |
| 6333/6334 | qdrant | Vector DB |

**Agent node (typical GPU node):**

| Port | Service | Notes |
|---|---|---|
| 8090 | tokenhub | Local LLM router |
| 6333/6334 | qdrant | Local vector DB |
| 11434 | ollama | Local model server |
| 8792 | whisper-server | Speech-to-text (if installed) |
| 7233 | Temporal | Workflow engine (if installed) |

---

## 15. Authentication

- **All API requests:** Require `Authorization: Bearer <token>` header.
- **Agent tokens:** Format `ccc-agent-<name>-<hex>`. Stored in the hub's secrets store; distributed via bootstrap or secrets-sync.
- **Bootstrap tokens:** One-time use, issued by the operator, consumed by `bootstrap.sh` to receive an agent token.
- **Auth DB:** `~/.ccc/auth.db` (SQLite) — stores hashed tokens. Used for user-level auth (distinct from agent tokens).
- **Dev mode:** If no tokens are configured, the server accepts all requests (unsafe; not for production).

---

## 16. Qdrant (Vector Search)

Qdrant runs on port 6333/6334. On the hub it serves fleet-wide vector search. On agent nodes it serves local semantic memory for the hermes runtime.

Hermes uses Qdrant alongside `memory_store.db` for semantic memory retrieval. Management scripts for ingesting sessions and querying memories are in `scripts/qdrant-python/`.

---

## 17. Ollama (Local Inference — GPU nodes)

On GPU-equipped agent nodes, Ollama serves local models for inference without cloud API calls. An `ollama-watchdog.sh` cron (typically every 15 minutes) keeps the server alive.

**Observed models on a GPU agent node:** `qwen2.5-coder:32b`, `qwen3-coder:latest`, `nomic-embed-text:latest`.

Ollama is not installed on the hub node (which uses cloud APIs via TokenHub).

---

## 18. Messaging Channels

Hermes handles all messaging channels. CCC itself does not implement channel connectors — it provides the workqueue and bus infrastructure; channel delivery is hermes's responsibility.

**Channels observed in running deployments:**
- **Slack** — Multiple workspace bot tokens per node, configured via `SLACK_BOT_TOKEN`/`SLACK_APP_TOKEN` in `.env`
- **Telegram** — `TELEGRAM_TOKEN` env var; active status node-dependent
- **Mattermost** — Configured at `chat.yourmom.photos` on some nodes

---

## 19. Memory System

### Per-agent (hermes)

Each hermes instance maintains its own memory independently:
- `~/.hermes/state.db` — Full conversation/session history (SQLite)
- `~/.hermes/memory_store.db` — Structured agent memory (SQLite)
- `~/.hermes/memories/` — Individual memory files
- `~/.hermes/MEMORY.md` — Human-readable cross-session facts

### Shared (git)

`memory-git-commit.sh` commits `MEMORY.md` and `memory/` to the CCC git repo daily. All nodes pull these changes via `agent-pull.sh`, giving agents a shared, git-versioned memory layer.

### MEMORY.md Schema

The seeded `MEMORY.md` (written by `setup-node.sh`) uses five typed memory networks:

| Network | Content | Confidence tracked |
|---|---|---|
| World Knowledge | Verified, stable facts about the fleet | No |
| Beliefs | Operational heuristics with confidence scores | Yes (0.4–0.8+) |
| Experiences | Work session outcomes | No |
| Reflections | Synthesized cross-session patterns | No |
| Entities | Profiles of nodes, services, systems | No |

Belief confidence: 0.4 = tentative, 0.6 = moderate, 0.8+ = strong. Decay: −0.1 on contradiction, entries pruned below 0.2. Confidence and last-updated date tracked per entry.

---

## 20. CCC-Node Skill

The `ccc-node` skill (`skills/ccc-node/SKILL.md`) is installed into hermes on each agent node and provides fleet connectivity procedures.

**Required environment:**
- `CCC_URL` — Hub API base URL
- `CCC_AGENT_TOKEN` — Agent bearer token
- `AGENT_NAME` — This agent's registered name

**Provides HTTP procedures for:**
- Posting heartbeats
- Pulling and updating workqueue items
- Sending and receiving ClawBus messages
- Fetching secrets from the hub
- Posting remote execution results

---

## 21. Observed Runtime State (Audit Snapshot, 2026-04-15)

This section records what was actually running on the inspected nodes. It is a point-in-time snapshot, not a normative description.

### Hub Node

**Active services:** ccc-server (port 8789), dashboard-server (port 8790), qdrant (6333/6334), minio (9000 localhost), tokenhub (8090), ccc-agent listen, agentfs-sync, hermes-agent, claude (Claude Code CLI session)

**Cron:** `agent-pull.sh` every 10 minutes; `memory-git-commit.sh` daily at midnight

**Data sizes:** queue.json ~2.5 MB, bus.jsonl ~118 KB (190 messages), exec.jsonl ~23 KB; hermes `state.db` ~111 MB

**Consul:** Installed by migrations 0009/0010, torn down by migration 0014 — not in use

**Additional processes not part of CCC core:** An unrelated personal API service and a reverse-proxy process for routing to other nodes (these are independent services sharing the host)

### GPU Agent Node (ARM64, NVIDIA GB10)

**Active services:** hermes-agent, qdrant (6333/6334), ollama (local: qwen2.5-coder:32b, qwen3-coder, nomic-embed-text), tokenhub (containerized), whisper-server (8792), ollama-watchdog process, Temporal workflow engine (7233, containerized)

**Cron:** `agent-pull.sh` every 10 minutes; `ollama-watchdog.sh` every 15 minutes; `memory-git-commit.sh` daily (appears three times — duplicate crontab entries)

**hermes `state.db`:** ~11 MB

**Additional processes:** Two unidentified Node.js services (ports 8791/8793) and a Python process (9876) not identifiable from this codebase; likely from other installed software on the host.
