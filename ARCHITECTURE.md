# Rocky Command Center — Architecture

## What We're Building

A distributed AI agent coordination platform designed around the realities of how people actually have access to compute and AI:

- No Anthropic API keys (too expensive / not available)
- Gold-standard intelligence = Claude/Codex CLI running in a tmux session, logged in by a human via SSO
- Middle intelligence = NVIDIA portal inference keys (rate-limited, slow, but real)
- Agents run on whatever hardware people have: home PCs, DGX Sparks, GPU cloud containers (outbound-only), full VMs
- The control plane must never assume inbound connectivity to agent nodes

---

## Components

### 1. Rocky Command Center (RCC) — The Hub

A cheap Azure CPU VM. The hub of all coordination.

**Role:** Resilient-intelligent coordinator. Not dumb — uses NVIDIA inference for real reasoning — but hardened to never give up.

**Key properties:**
- Every LLM call is a queued message with a master clock
- If model stops responding: retry → try fallback model → back off → retry
- Model fallback chain: Claude Sonnet → Llama 70B → Nemotron → (back off) → retry
- Rate-limit aware: leaky bucket per model per minute, never saturates
- The one component that **cannot** go dark. Agent death is recoverable (work lease expires). Hub death means nobody knows anything.
- Exposes a health endpoint: `GET /health` → `{ok, lastLlmResponseAge, queueDepth, agents}`

**Services running on RCC:**
- `rcc-api` — REST API (HTTPS only), auth via user tokens
- `rcc-dashboard` — Web UI (Rocky Command Center dashboard)
- `rcc-brain` — The LLM request queue + retry engine
- `rcc-bus` — SquirrelBus message routing (agent ↔ agent via hub)
- `rcc-storage` — Storage tier abstraction (public Azure Blob / private Azure / local MinIO proxy)
- `rcc-watchdog` — Monitors agent heartbeats, escalates stale agents to jkh

---

### 2. Agent Nodes — The Spokes

Anything with a Claude/Codex CLI session in tmux, or any machine running OpenClaw.

**Key properties:**
- Outbound-only connectivity is sufficient — agents reach out to RCC, RCC doesn't reach in
- Intelligence: Claude CLI (interactive, SSO-auth'd by human) or NVIDIA keys (batch/async)
- Registration: agent runs `rocky register <rcc-url> <token>` → gets its own agent token
- Heartbeat: POST to RCC every N minutes with status, queue depth, GPU util, etc.
- Work lease: when claiming a work item, lease expires after TTL — another agent can reclaim

**Agent types:**
- `full` — Full VM, inbound+outbound, can run SquirrelBus receive endpoint
- `container` — GPU container, outbound-only, polls RCC for messages
- `local` — Home PC/desktop, NAT'd, polls RCC
- `spark` — DGX Spark, treated like `local` unless network allows more

---

### 3. Storage Tiers

| Tier | Where | Access | Use for |
|------|-------|--------|---------|
| Public | Azure Blob Storage | Internet (HTTPS, read-only public) | Published assets, renders, public docs |
| Private Cloud | Azure Blob Storage | HTTPS + SAS token | In-progress work, agent-to-agent file transfer via cloud |
| Local/Fast | MinIO (on RCC or agent cluster) | Internal network + key | High-speed inter-agent storage, queue state, logs |

RCC's `rcc-storage` service abstracts all three tiers behind a single API. Agents don't need to know which tier they're talking to.

---

### 4. Human Operators

Each human gets:
- A unique `username` + `bearer token` (HTTPS only, never in URLs)
- Optional: one or more SSH public keys → SSH access to RCC for CLI
- Role: `owner` (first human, can add/remove others) or `collaborator`

The "boss" (owner) can invite collaborators. Each collaborator can see the dashboard, post to the work queue, and message agents. Only the owner can add/remove agents and other humans.

---

### 5. Agent Registration Flow

```
Human installs OpenClaw on GPU machine
Human runs: rocky register https://rcc.example.com
Rocky CLI prompts for RCC token
RCC creates agent record, returns agent token
Agent stores token, starts heartbeat cron
Agent appears on RCC dashboard
```

For Claude CLI agents (the gold standard):
```
Human opens tmux session
Human runs: openclaw start (or zeroclaw, etc.)
Human does /login → gets URL → pastes into browser → pastes credential back
Agent is now online and full-intelligence
Human runs: rocky register https://rcc.example.com
```

RCC doesn't automate the SSO login. It accepts the agent once it's running.

---

## The Nervous System

RCC's `rcc-brain` is not just a message queue — it's an autonomous loop:

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
  7. Route any pending SquirrelBus messages
  8. Sleep(tick_interval)
```

The tick interval is tunable. Default: 30s. Under load: 5s.

State is persisted to disk after every tick. If rcc-brain crashes and restarts, it picks up exactly where it left off.

---

## What We're NOT Building

- We are not building an LLM serving layer — the agents bring their own
- We are not building SSO — humans log agents in manually
- We are not assuming inbound connectivity to agent nodes
- We are not assuming all agents share a network segment
- We are not assuming any single component is always available

---

## Repo Structure (target)

```
rocky/
├── rcc/                    # Rocky Command Center services
│   ├── api/                # REST API server
│   ├── brain/              # LLM queue + retry engine
│   ├── bus/                # SquirrelBus routing
│   ├── dashboard/          # Web UI (evolving from current dashboard/)
│   ├── storage/            # Storage tier abstraction
│   └── watchdog/           # Agent health monitor
├── cli/                    # `rocky` CLI (register, status, send)
├── agent/                  # Agent-side runtime (heartbeat, work processor)
├── workqueue/              # Queue schema, spec, agent instructions
├── squirrelbus/            # Bus protocol spec + plugin
├── lib/                    # Shared utilities
├── dashboard/              # Current dashboard (being refactored into rcc/dashboard)
├── docs/                   # Architecture docs, setup guides
│   └── ARCHITECTURE.md     # This file
└── deploy/                 # Azure deployment scripts, systemd units, etc.
```

---

## Immediate Next Steps

1. **`rcc/brain/`** — Build the LLM queue + retry engine. This is the nervous system. Nothing else matters if this isn't solid.
2. **`cli/rocky`** — Build the `rocky register` command so new agents can onboard without jkh doing it manually.
3. **Refactor `dashboard/`** → `rcc/dashboard/` — Current dashboard is the seed of the UI. Move and clean up.
4. **`rcc/storage/`** — Thin abstraction over the three storage tiers.
5. **`deploy/`** — Azure ARM template or Bicep for one-click RCC deploy on a cheap CPU VM.

---

*Last updated: 2026-03-21 by Rocky 🐿️*

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
 └── RCC (Rocky) — nervous system, queue, routing
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
RCC routes to the right worker based on task type.

### Agent Registry Schema (capabilities)

When an agent registers with RCC, it declares its capabilities:

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

RCC dispatch uses `preferred_executor` on each work item:

| Task type | Preferred executor | Reason |
|-----------|-------------------|--------|
| Heartbeat / queue poll | `inference_key` | Trivial, metered cost acceptable |
| Simple status update | `inference_key` | Same |
| Complex reasoning / code gen | `claude_cli` | Fixed cost, powerful |
| Multi-step orchestration | `claude_cli` | Parallel subagents, no token anxiety |
| GPU render / simulation | `gpu` (direct) | Not an LLM job at all |
| Routing decision (RCC brain) | `inference_key` (with fallback chain) | Hub must always work even if CLIs are down |

**The golden rule:** Never route expensive reasoning to a metered agent without
explicit override. Never route GPU work to an LLM. Keep the hub's inference key
usage lean so it can always coordinate even when everything else is down.
