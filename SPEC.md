# CCC System Specification

**Version:** Audited 2026-04-15  
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
- `hermes-agent` — AI agent runtime
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

All routes confirmed present in source (`ccc/dashboard/ccc-server/src/routes/`):

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
| GET | `/api/bus/stream` | SSE stream — real-time message delivery |
| POST | `/api/bus/send` | Send message to bus |
| GET | `/api/bus/messages` | Query messages (params: `limit`, `subject`, `type`, `thread_id`, `to`, `from`) |
| GET | `/api/bus/presence` | Agent presence (online/offline status) |

All four routes are duplicated under `/bus/*` for reverse-proxy compatibility (e.g., nginx forwarding `/bus/` → port 8789).

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
  "claimedBy": null | "<agent-name>",
  "claimedAt": null | "ISO-8601",
  "attempts": 0,
  "maxAttempts": 3,
  "completedAt": null | "ISO-8601",
  "result": null | "summary text"
}
```

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

## 5. ClawBus (Message Bus)

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

## 6. Remote Execution (ccc-agent listen)

The exec-listener daemon runs on each node and enables remote code execution dispatched from the hub.

**How it works:**
1. `ccc-agent listen` subscribes to the hub's SSE stream at `/api/bus/stream`
2. On receiving a message with `type: "rcc.exec"`, it parses: `{execId, code, target, mode, sig}`
3. Executes the code via `/bin/sh`
4. Posts result to `/api/exec/<execId>/result`: `{"result": "...", "exitCode": 0}`

All results are logged to `exec.jsonl` on the hub. Observed size on the running hub: ~23 KB.

On hub nodes this typically runs as a systemd service (`ccc-exec-listen.service`). On agent nodes it may run via cron or be started by the agent runtime.

---

## 7. ccc-agent CLI

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

## 8. Agent Runtime: hermes-agent

Hermes is the primary AI agent runtime on all nodes. It replaced OpenClaw as the standard runtime. Agents are identified by `AGENT_NAME` in their environment.

**Binary:** Python, installed from `github.com/jordanhubbard/hermes-agent` via `pipx` or a venv.

**Per-node data at `~/.hermes/`:**

| File/Dir | Purpose |
|---|---|
| `config.yaml` | LLM provider config, agent personality, session settings |
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

**Size observed in practice:** `state.db` grows to 10–111 MB on active nodes depending on session history volume.

**LLM routing:** Hermes supports multiple provider backends. In the observed deployments, model requests are routed through NVIDIA's inference API for Claude models, and through local TokenHub for routing across backends. Claude Code CLI (when running alongside hermes) also routes through the same NVIDIA endpoint via `ANTHROPIC_BASE_URL`.

**Skills observed in running instances:** 25–31 skills per node, covering software development, devops, data science, research, media, and fleet infrastructure (`ccc-node` skill).

---

## 9. TokenHub (LLM Router)

TokenHub is an LLM routing proxy that aggregates multiple inference backends and exposes a unified OpenAI-compatible API at port 8090.

Backends routed include cloud inference APIs (NVIDIA, others) and local model servers (Ollama, vLLM). Agents configure `TOKENHUB_URL` to point to a local or hub-hosted TokenHub instance.

On hub nodes TokenHub runs as a standalone systemd service. On GPU agent nodes it may run inside a container.

---

## 10. agentfs-sync

`agentfs-sync` is a daemon that mirrors the workspace to/from MinIO (S3) at the hub. It provides durable shared storage so agents can access workspace state without direct git access.

Runs as a systemd service (`agentfs-sync.service`) on the hub. Not required on agent nodes that have direct git access.

---

## 11. dashboard-server

A Rust binary that provides the operator dashboard UI. Observed startup log:

```
RCC Dashboard v2 starting port=8790 rcc_url=http://localhost:8789 sc_url=http://localhost:8793 operator=jkh
```

It proxies requests to `ccc-server` (8789) and optionally to SquirrelChat (8793, a separate chat service that has its own codebase not in this repo).

**Alternative frontend:** The codebase also contains a WASM/Leptos SPA in `ccc/dashboard/clawchat/` that `ccc-server` can serve directly as a fallback when `DASHBOARD_DIST` is set. This is a different path from the `dashboard-server` binary. As of audit, the binary approach is what's running.

---

## 12. Deploy and Maintenance

### Continuous Pull (agent-pull.sh)

Runs every 10 minutes via cron on all nodes. Steps:
1. `git pull --ff-only origin <branch>` — update workspace repo
2. Detect changed files; restart affected services if deployment-relevant files changed
3. `npm install` if `package.json` changed
4. Sync secrets from hub via `secrets-sync.sh`
5. Post heartbeat to hub with current git revision and hardware info

### Memory Commit (memory-git-commit.sh)

Runs daily at midnight via cron. Stages `MEMORY.md` and `memory/` directory and creates a git commit if there are changes. Provides git-backed time-travel history of agent memory that propagates to all nodes via `agent-pull.sh`.

### Migrations (deploy/migrations/)

13 numbered shell scripts (0001–0013), applied via `bash deploy/run-migrations.sh`. State tracked in `~/.ccc/migrations.json`. Each script is idempotent and safe to re-run.

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

**Note:** Consul (migrations 0009/0010) is configured but was not running on any inspected node at audit time.

### Bootstrap (bootstrap.sh)

One-command onboarding for new agent nodes:
1. Install hermes-agent (pipx preferred, pip3 fallback)
2. Clone CCC workspace to `~/.ccc/workspace`
3. Call bootstrap API to consume a one-time token and receive an agent token + secrets bundle
4. Write `~/.ccc/.env` with all credentials (including any Slack/Telegram channel tokens)
5. Install agentfs-sync if available from MinIO
6. Configure vLLM if a GPU is detected (`nvidia-smi`)
7. Install `ccc-node` skill into hermes
8. Collect hardware fingerprint, post heartbeat and capabilities to hub
9. Write `~/.ccc/agent.json` (onboarding signature with version and timestamp)

### setup-node.sh

Idempotent node setup script (as opposed to first-time bootstrap). Installs:
- Pull cron (every 10 minutes)
- Ops crons (memory-commit daily at midnight)
- Hermes agent runtime (same install logic as bootstrap)
- ccc-node skill into hermes
- Seeded `MEMORY.md` with typed-network fleet context (if not already present)
- vLLM setup if GPU detected

---

## 13. Networking

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

## 14. Authentication

- **All API requests:** Require `Authorization: Bearer <token>` header.
- **Agent tokens:** Format `ccc-agent-<name>-<hex>`. Stored in the hub's secrets store; distributed via bootstrap or secrets-sync.
- **Bootstrap tokens:** One-time use, issued by the operator, consumed by `bootstrap.sh` to receive an agent token.
- **Auth DB:** `~/.ccc/auth.db` (SQLite) — stores hashed tokens. Used for user-level auth (distinct from agent tokens).
- **Dev mode:** If no tokens are configured, the server accepts all requests (unsafe; not for production).

---

## 15. Qdrant (Vector Search)

Qdrant runs on port 6333/6334. On the hub it serves fleet-wide vector search. On agent nodes it serves local semantic memory for the hermes runtime.

Hermes uses Qdrant alongside `memory_store.db` for semantic memory retrieval. Management scripts for ingesting sessions and querying memories are in `scripts/qdrant-python/`.

---

## 16. Ollama (Local Inference — GPU nodes)

On GPU-equipped agent nodes, Ollama serves local models for inference without cloud API calls. An `ollama-watchdog.sh` cron (typically every 15 minutes) keeps the server alive.

**Observed models on a GPU agent node:** `qwen2.5-coder:32b`, `qwen3-coder:latest`, `nomic-embed-text:latest`.

Ollama is not installed on the hub node (which uses cloud APIs via TokenHub).

---

## 17. Messaging Channels

Hermes handles all messaging channels. CCC itself does not implement channel connectors — it provides the workqueue and bus infrastructure; channel delivery is hermes's responsibility.

**Channels observed in running deployments:**
- **Slack** — Multiple workspace bot tokens per node, configured via `SLACK_BOT_TOKEN`/`SLACK_APP_TOKEN` in `.env`
- **Telegram** — `TELEGRAM_BOT_TOKEN` env var; active status node-dependent
- **Mattermost** — Configured at `chat.yourmom.photos` on some nodes

---

## 18. Memory System

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

## 19. CCC-Node Skill

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

## 20. Observed Runtime State (Audit Snapshot, 2026-04-15)

This section records what was actually running on the inspected nodes. It is a point-in-time snapshot, not a normative description.

### Hub Node

**Active services:** ccc-server (port 8789), dashboard-server (port 8790), qdrant (6333/6334), minio (9000 localhost), tokenhub (8090), ccc-agent listen, agentfs-sync, hermes-agent, claude (Claude Code CLI session)

**Cron:** `agent-pull.sh` every 10 minutes; `memory-git-commit.sh` daily at midnight

**Data sizes:** queue.json ~2.5 MB, bus.jsonl ~118 KB (190 messages), exec.jsonl ~23 KB; hermes `state.db` ~111 MB

**Consul:** Installed (migrations 0009/0010) but not running

**Additional processes not part of CCC core:** An unrelated personal API service and a reverse-proxy process for routing to other nodes (these are independent services sharing the host)

### GPU Agent Node (ARM64, NVIDIA GB10)

**Active services:** hermes-agent, qdrant (6333/6334), ollama (local: qwen2.5-coder:32b, qwen3-coder, nomic-embed-text), tokenhub (containerized), whisper-server (8792), ollama-watchdog process, Temporal workflow engine (7233, containerized)

**Cron:** `agent-pull.sh` every 10 minutes; `ollama-watchdog.sh` every 15 minutes; `memory-git-commit.sh` daily (appears three times — duplicate crontab entries)

**hermes `state.db`:** ~11 MB

**Additional processes:** Two unidentified Node.js services (ports 8791/8793) and a Python process (9876) not identifiable from this codebase; likely from other installed software on the host.
