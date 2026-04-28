# Agent Control Center — Architecture

## What We're Building

A distributed AI agent coordination platform designed around the realities of how people actually have access to compute and AI:

- No Anthropic API keys (too expensive / not available)
- Gold-standard intelligence = Claude/Codex CLI running in a tmux session, logged in by a human via SSO
- Middle intelligence = NVIDIA portal inference keys (rate-limited, slow, but real)
- Agents run on whatever hardware people have: home PCs, DGX Sparks, GPU cloud containers (outbound-only), full VMs
- The control plane must never assume inbound connectivity to agent nodes

---

## Components

### 1. Agent Control Center (ACC) — The Hub

A cheap Azure CPU VM. The hub of all coordination.

**Role:** Resilient-intelligent coordinator. Not dumb — uses NVIDIA inference for real reasoning — but hardened to never give up.

**Key properties:**
- Every LLM call is a queued message with a master clock
- If model stops responding: retry → try fallback model → back off → retry
- Model fallback chain: Claude Sonnet → Llama 70B → Nemotron → (back off) → retry
- Rate-limit aware: leaky bucket per model per minute, never saturates
- The one component that **cannot** go dark. Agent death is recoverable (work lease expires). Hub death means nobody knows anything.
- Exposes a health endpoint: `GET /health` → `{ok, lastLlmResponseAge, queueDepth, agents}`

**Services running on CCC:**
- `acc-api` — REST API (HTTPS only), auth via user tokens
- `acc-dashboard` — Web UI (Agent Control Center dashboard)
- `acc-brain` — The LLM request queue + retry engine
- `acc-bus` — AgentBus message routing (agent ↔ agent via hub)
- `acc-storage` — Storage tier abstraction (public Azure Blob / private Azure / local MinIO proxy)
- `acc-watchdog` — Monitors agent heartbeats, escalates stale agents to jkh

---

### 2. Agent Nodes — The Spokes

Anything with a Claude/Codex CLI session in tmux, or any machine running hermes-agent.

**Key properties:**
- Outbound-only connectivity is sufficient — agents reach out to CCC, CCC doesn't reach in
- Intelligence: Claude CLI (interactive, SSO-auth'd by human) or NVIDIA keys (batch/async)
- Registration: agent runs `rocky register <ccc-url> <token>` → gets its own agent token
- Heartbeat: POST to CCC every N minutes with status, queue depth, GPU util, etc.
- Work lease: when claiming a work item, lease expires after TTL — another agent can reclaim

**Agent types:**
- `full` — Full VM, inbound+outbound, can run AgentBus receive endpoint
- `container` — GPU container, outbound-only, polls CCC for messages
- `local` — Home PC/desktop, NAT'd, polls CCC
- `spark` — DGX Spark, treated like `local` unless network allows more

---

### 3. Storage Tiers

| Tier | Where | Access | Use for |
|------|-------|--------|---------|
| Public | Azure Blob Storage | Internet (HTTPS, read-only public) | Published assets, renders, public docs |
| Private Cloud | Azure Blob Storage | HTTPS + SAS token | In-progress work, agent-to-agent file transfer via cloud |
| Local/Fast | MinIO (on CCC or agent cluster) | Internal network + key | High-speed inter-agent storage, queue state, logs |

CCC's `acc-storage` service abstracts all three tiers behind a single API. Agents don't need to know which tier they're talking to.

---

### 4. Human Operators

Each human gets:
- A unique `username` + `bearer token` (HTTPS only, never in URLs)
- Optional: one or more SSH public keys → SSH access to CCC for CLI
- Role: `owner` (first human, can add/remove others) or `collaborator`

The "boss" (owner) can invite collaborators. Each collaborator can see the dashboard, post to the work queue, and message agents. Only the owner can add/remove agents and other humans.

---

### 5. Agent Registration Flow

```
Install hermes-agent on the node (pipx install hermes-agent)
Copy deploy/.env.template → ~/.ccc/.env, fill in CCC_URL + CCC_AGENT_TOKEN
Run: make register   (POSTs capabilities to the hub)
Agent appears on CCC dashboard
```

For Claude CLI agents:
```
Start a persistent tmux session: tmux new-session -d -s claude-main
Launch Claude Code: tmux send-keys -t claude-main 'claude --dangerously-skip-permissions' Enter
Set AGENT_CLAUDE_CLI=true in ~/.ccc/.env
Run: make register
```

CCC doesn't manage the Claude Code session — it delegates to it via claude-worker.mjs.

---

## The Nervous System

CCC's `acc-brain` is not just a message queue — it's an autonomous loop:

```
while true:
  1. Check work queue for items needing LLM reasoning
  2. For each: wrap in retry envelope with master clock
  3. POST to current model (respecting leaky bucket)
  4. On success: record response, advance clock, continue
  5. On timeout/429: 
       - increment retry counter
       - if retries < 3: wait backoff, retry same model
       - if retries >= 3: try next model in fallback chain
       - if all models exhausted: mark item as "llm-unavailable", alert watchdog
  6. Check agent heartbeats — escalate stale agents
  7. Route any pending AgentBus messages
  8. Sleep(tick_interval)
```

The tick interval is tunable. Default: 30s. Under load: 5s.

State is persisted to disk after every tick. If acc-brain crashes and restarts, it picks up exactly where it left off.

---

## What We're NOT Building

- We are not building an LLM serving layer — the agents bring their own
- We are not building SSO — humans log agents in manually
- We are not assuming inbound connectivity to agent nodes
- We are not assuming all agents share a network segment
- We are not assuming any single component is always available

---

## Shared Client Libraries

Drift between the three HTTP clients (`acc-cli`, `acc-agent`, hermes
`acc_shared_memory` plugin) was eliminated by extracting shared libraries.

### Rust

The Cargo workspace at the repo root unifies all five Rust crates:

| Crate | Path | Role |
|-------|------|------|
| `acc-model` | `acc-model/` | Shared wire types (tasks, bus, memory, projects, queue, agents) |
| `acc-client` | `acc-client/` | Async HTTP client; used by `acc-cli` and `acc-agent` |
| `acc-cli` | `acc-cli/` | Fleet CLI binary |
| `acc-server` | `acc-server/` | Hub REST API server |
| `acc-agent` | `agent/acc-agent/` | Agent-side runtime |

**`acc-model`** — Zero-logic, zero-network, zero-async. Pure `serde` structs
that any crate in the tree can depend on.  The server's internal representation
diverges from the wire shape in places (e.g. richer status enums, audit fields)
but those stay in `acc-server`; `acc-model` carries only what crosses the HTTP
boundary.

**`acc-client`** — Async `reqwest`-based client.  Notable design choices:
- SSE bus streaming (`BusApi::stream`) returns `impl Stream<Item = Result<BusMsg>>`
  so callers drive it with `StreamExt::next().await` and own the reconnect policy.
- Malformed JSON in a single SSE frame is silently skipped; the stream continues.
- `Client::from_env()` resolves `ACC_HUB_URL` → `ACC_TOKEN` (same precedence as
  the Python client).

**`acc-agent` — lib+bin crate** — The agent runtime is structured as both a
library crate (`acc_agent`) and a binary (`acc-agent`) from the same source
tree.  This is intentional:

| File | Role |
|------|------|
| `src/lib.rs` | Re-exports every module as `pub mod`; this is the library root |
| `src/main.rs` | Binary entry point; imports via `use acc_agent::{…}` — declares no modules itself |
| `Cargo.toml` `[lib]` | `name = "acc_agent"`, `path = "src/lib.rs"` |
| `Cargo.toml` `[[bin]]` | `name = "acc-agent"`, `path = "src/main.rs"` |

**Why the split?** Integration tests in `peer_exchange.rs` and `queue.rs` need
access to `hub_mock` and other internal types.  A pure binary crate can only
host tests inside itself and cannot be imported by external test harnesses or
other workspace crates.  Promoting the crate to lib+bin fixes this without any
change to the compiled binary's behaviour or public CLI surface.

### Python

The Python package lives at `clients/python/acc_client/` and is installable via:

```bash
pip install -e clients/python/acc_client
```

It mirrors the Rust `acc-client` shape so callers can reason about both the same
way.  Key behaviors:
- `Client.from_env()` resolves `ACC_HUB_URL` / `ACC_URL` / `CCC_URL` for the base
  URL (same precedence as the Rust client).
- Token precedence: explicit arg → `ACC_TOKEN` → `CCC_AGENT_TOKEN` → `ACC_AGENT_TOKEN`
  → `~/.acc/.env`.
- `client.bus.send("kind", from_="agent")` — the `from_` kwarg is mapped to the
  wire field `"from"` to avoid shadowing the Python built-in.
- `client.bus.stream()` — synchronous SSE generator; reconnect at the call site.

The hermes `acc_shared_memory` plugin (`hermes/contrib/plugins/acc_shared_memory/`)
consumes the Python client directly.

---

## Slack Fleet Reporter

`agent/acc-agent/src/slack.rs` posts compact emoji-led messages to a Slack
channel on every task claim, completion, and failure.  It is entirely
opt-in: if `SLACK_BOT_TOKEN` is absent the functions return immediately and
nothing is posted.

### Required environment variables

| Variable | Description | Default |
|---|---|---|
| `SLACK_BOT_TOKEN` | Bot OAuth token (`xoxb-…`). **Required** to enable posting. Without it the reporter is a silent no-op. | — |
| `SLACK_FLEET_CHANNEL` | Channel ID or name to receive fleet events. | `fleet-activity` |

`ACC_URL` is also read (if present) to build a one-click deep-link to the hub
dashboard in each message; it is optional.

### How to get a bot token

1. Create a Slack app at <https://api.slack.com/apps> and add the
   `chat:write` OAuth scope under *Bot Token Scopes*.
2. Install the app to your workspace; copy the **Bot User OAuth Token**
   (`xoxb-…`).
3. Invite the bot to the target channel:
   `/invite @your-bot-name` inside Slack.
4. Set `SLACK_BOT_TOKEN=xoxb-…` and (optionally) `SLACK_FLEET_CHANNEL=<id>`
   in `~/.acc/.env`.

Both variables are included in `deploy/.env.template` under the
`# ── Messaging ──` section.

---

## Repo Structure (target)

```
rocky/
├──.acc/                    # Agent Control Center services
│   ├── api/                # REST API server
│   ├── brain/              # LLM queue + retry engine
│   ├── bus/                # AgentBus routing
│   ├── dashboard/          # Web UI (evolving from current dashboard/)
│   ├── storage/            # Storage tier abstraction
│   └── watchdog/           # Agent health monitor
├── cli/                    # `rocky` CLI (register, status, send)
├── agent/                  # Agent-side runtime (heartbeat, work processor)
├── workqueue/              # Queue schema, spec, agent instructions
├── squirrelbus/            # Bus protocol spec + plugin
├── lib/                    # Shared utilities
├── docs/                   # Architecture docs, setup guides
│   └── ARCHITECTURE.md     # This file
└── deploy/                 # Azure deployment scripts, systemd units, etc.
```

---

## Current Status (as of 2026-04-02)

Core infrastructure is operational. The "immediate next steps" from March have shipped:

- ✅ **.ccc/brain/`** — LLM queue + retry engine live; fallback chain: Claude Sonnet → Llama 70B → Nemotron
- ✅ **.ccc/api/routes/`** — Monolithic `index.mjs` split into domain route modules
- ✅ **tokenhub** — LLM gateway (Go, OpenAI-compat, rate limiting, circuit breakers)

## Active Work Areas

1. **.ccc/brain/` edge cases** — All-models-degraded recovery, partial state replay under failure
2. **Fleet expansion** — New nodes join via `rocky register`; auto-provision from topology

---

*Last updated: 2026-04-02 by Snidely 🎩*

---

## Agent Capability Model (updated 2026-03-21)

Every agent node has two potential "workers" attached to it:

### Worker Types

| Worker | Cost Model | Best For | Weakness |
|--------|-----------|----------|---------|
| **Claude CLI** (tmux) | Fixed monthly (~$20-100) | Complex reasoning, parallel subagents, long tasks, code gen | Requires human SSO auth, can die/need reauth |
| **Inference Key** (NVIDIA/OpenAI) | Per token (metered) | Hub API calls, heartbeats, queue polling, simple coordination | Expensive at scale, rate-limited, no parallelism |
| **GPU** (direct) | Fixed (hardware/cloud cost) | Renders, simulation, inference, training | Not LLM — for actual compute work |

### Topology

```
jkh
 └── CCC (Rocky) — nervous system, queue, routing
      ├── Rocky's Claude CLI (tmux) — fixed cost, parallel
      ├── Bullwinkle node
      │    └── Bullwinkle's Claude CLI (tmux) — fixed cost, parallel
      ├── Natasha node  
      │    └── Natasha's Claude CLI (tmux) — fixed cost, parallel
      │    └── Blackwell GPU — render/inference compute
      └── Boris node
           └── Boris's Claude CLI (tmux) — fixed cost, parallel
           └── Dual L40 GPUs — render/simulation compute
```

**Key insight:** Each agent node IS its own mini-hub. The inference key handles
coordination/API traffic. The Claude CLI handles the actual intelligent work.
CCC routes to the right worker based on task type.

### Agent Registry Schema (capabilities)

When an agent registers with CCC, it declares its capabilities:

```json
{
  "name": "boris",
  "host": "sweden-l40",
  "type": "full",
  "capabilities": {
    "claude_cli": true,
    "claude_cli_model": "claude-sonnet-4-6",
    "inference_key": true,
    "inference_provider": "nvidia",
    "gpu": true,
    "gpu_model": "L40",
    "gpu_count": 2,
    "gpu_vram_gb": 96
  },
  "billing": {
    "claude_cli": "fixed",
    "inference_key": "metered",
    "gpu": "fixed"
  }
}
```

### Routing Rules

CCC dispatch uses `preferred_executor` on each work item:

| Task type | Preferred executor | Reason |
|-----------|-------------------|--------|
| Heartbeat / queue poll | `inference_key` | Trivial, metered cost acceptable |
| Simple status update | `inference_key` | Same |
| Complex reasoning / code gen | `claude_cli` | Fixed cost, powerful |
| Multi-step orchestration | `claude_cli` | Parallel subagents, no token anxiety |
| GPU render / simulation | `gpu` (direct) | Not an LLM job at all |
| Routing decision (CCC brain) | `inference_key` (with fallback chain) | Hub must always work even if CLIs are down |

**The golden rule:** Never route expensive reasoning to a metered agent without
explicit override. Never route GPU work to an LLM. Keep the hub's inference key
usage lean so it can always coordinate even when everything else is down.
