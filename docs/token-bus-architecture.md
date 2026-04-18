# Token Bus Architecture

**Status**: Design / RFC  
**Date**: 2026-03-28  
**Author**: Natasha (from jkh directive)

---

## The Problem

All agents (Natasha, Bullwinkle, Rocky, Boris, etc.) currently consume tokens from
`inference.nvidia.com` — a metered, per-token-cost endpoint. Boris now has a vLLM
server running Nemotron-3 120B locally, tunneled into do-host1. This is **fixed cost**.

The bootstrapping chicken/egg:
> An agent needs "local intelligence" (tokens) to even know how to route to a token
> provider. You can't eliminate an agent's dependency on *some* token provider just
> to bootstrap routing decisions.

So the question isn't "can we eliminate the metered endpoint" — it's:
**"Can we minimize metered token use to the bare minimum needed for routing/bootstrap,
while routing bulk inference to fixed-cost providers?"**

---

## Three Actor Types

### 1. Token Consumer
- Examples: Natasha, Bullwinkle, Rocky, Boris (as agent)
- Has: An OpenClaw instance, a system prompt, tools, a personality
- Needs: An OpenAI-compatible token endpoint to function
- Produces: Agent actions, AgentBus messages, work queue completions

### 2. Token Provider  
- Examples: Boris's vLLM server (Nemotron-3 120B)
- Has: GPU(s), vLLM stack, a model loaded
- Needs: A way to advertise itself on the bus and be reachable
- Produces: `/v1/chat/completions`, `/v1/models` — OpenAI-compatible API

### 3. CCC Central Command
- Running on: hub node (CCC server host)
- Has: Public IP, port management, AgentBus, work queue, UI
- Does: Coordinates consumers ↔ providers, human ↔ agent interface
- Acts as: Registry, router hint, heartbeat monitor

---

## Proposed Design

### A. Provider Registration Protocol

When a token provider comes online:
1. SSH reverse tunnel to do-host1 (as `tunnel` user) — already working
2. POST to CCC: `PUT /api/providers/:id` with:
   ```json
   {
     "id": "boris-nemotron",
     "type": "vllm",
     "model": "nemotron",
     "local_port": 18080,
     "context_len": 262144,
     "owner": "boris",
     "status": "online"
   }
   ```
3. CCC binds a stable local port (e.g. `127.0.0.1:19000`) and proxies to tunnel port
4. CCC publishes to AgentBus: `provider.online` event
5. Consumers see new provider, can update their routing config

### B. Consumer Bootstrap (Solving the Chicken/Egg)

The insight: **You only need a *tiny* amount of metered intelligence to bootstrap.**

Proposed two-tier model:
- **Tier 0 (bootstrap)**: Metered endpoint (inference.nvidia.com). Used ONLY for:
  - Startup routing decisions ("which provider should I use?")
  - Fallback when no provider is available
  - Provider health-check interpretation
- **Tier 1 (bulk)**: Fixed-cost provider (Boris's Nemotron). Used for:
  - All normal agent Q&A
  - AgentBus message processing
  - Work queue tasks

The consumer agent config gains a `providers` section:
```json
{
  "model_routing": {
    "bootstrap": "inference.nvidia.com/claude-sonnet",
    "default": "http://127.0.0.1:18080/v1",
    "fallback": "inference.nvidia.com/claude-sonnet",
    "strategy": "prefer_local"
  }
}
```

OpenClaw's gateway would handle the routing transparently — agents don't change
their API calls, the gateway picks the endpoint.

### C. Provider Installer ("Make Boris Easy")

A canned setup script / playbook that:
1. Installs vLLM + CUDA deps
2. Downloads / mounts the model
3. Generates an SSH keypair (`boris-tunnel` pattern)
4. Asks CCC for a tunnel slot: `POST /api/tunnel/request`
5. CCC adds the pubkey to `tunnel`'s authorized_keys automatically
6. Starts vLLM + an CCC heartbeat agent
7. Provider is live on the bus

This is the "as many Borises as you like" path.

### D. CCC Provider Registry (new endpoints needed)

```
GET  /api/providers          → list all registered providers + status
PUT  /api/providers/:id      → register/update a provider
DELETE /api/providers/:id    → deregister
POST /api/tunnel/request     → request tunnel slot, returns port + adds pubkey
GET  /api/tunnel/pubkey      → get the pubkey to authorize (for manual installs)
```

---

## Implementation Phases

### Phase 1: Provider Registry in CCC (do now)
- Add `providers` table/store to CCC
- Add `/api/providers` CRUD endpoints
- Boris's tunnel auto-registers on connect (or manual POST for now)

### Phase 2: CCC Tunnel Automation
- `POST /api/tunnel/request` endpoint
- Accepts a pubkey, adds it to `tunnel`'s authorized_keys automatically
- Returns assigned port
- No more manual `useradd` dance

### Phase 3: Consumer Routing in OpenClaw Gateway
- Gateway reads provider list from CCC
- Routes based on `strategy: prefer_local` / `fallback` config
- Transparent to agent — same OpenAI API calls, different backend

### Phase 4: Provider Installer Script
- Single `curl | bash` or ansible playbook
- End-to-end: GPU box → registered provider on the bus

---

## Open Questions

1. **Auth between provider and consumers**: Should consumers need a token to hit
   the proxied vLLM? Or is being on the same host (localhost) sufficient?
2. **Load balancing**: Multiple Borises → round-robin? Least-loaded?
3. **Model capability routing**: Some tasks need Claude's reasoning; bulk Q&A can
   use Nemotron. Who decides? Static config or dynamic?
4. **Provider health monitoring**: vLLM crashes → CCC should detect + notify + 
   fall back consumers to Tier 0.

---

## Work Queue Tasks (to create)

- [ ] CCC: Add provider registry endpoints
- [ ] CCC: Add tunnel automation endpoint  
- [ ] CCC: Dashboard provider status widget
- [ ] OpenClaw: Gateway routing config support
- [ ] Boris installer: canned vLLM setup script
- [ ] Document: Provider onboarding guide

