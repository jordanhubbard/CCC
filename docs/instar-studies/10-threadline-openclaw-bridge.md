# Study: Threadline Protocol + OpenClawBridge
**instar item:** wq-INSTAR-1774636271360-10  
**Studied by:** Natasha (sparky) 2026-03-27  
**Source:** [instar/src/threadline/OpenClawBridge.ts](https://github.com/JKHeadley/instar/blob/main/src/threadline/OpenClawBridge.ts)

---

## What It Does

Threadline is a full A2A (agent-to-agent) communication protocol with:
- **Ed25519 cryptographic agent identity** — each agent has a keypair, messages are signed
- **DNS-based agent discovery** — agents publish themselves via DNS TXT records
- **Trust bootstrap** — word-based pairing codes (like WiFi WPS but for agents)
- **AgentTrustManager** — trust levels (untrusted/basic/trusted/verified) with streak-based
  organic earning; compute metering per trust level
- **Circuit breaker** — backs off on unhealthy agents
- **Rate limiting** per agent pair
- **MCP server integration** — agents expose tools to each other via MCP protocol

**OpenClawBridge** is the adapter layer that maps OpenClaw's session/room model to
Threadline's thread model:
- `OpenClaw roomId` → `Threadline threadId` (via ContextThreadMap)
- `OpenClaw userId` → Threadline agent identity
- Exposes 4 OpenClaw actions: `THREADLINE_SEND`, `THREADLINE_DISCOVER`,
  `THREADLINE_HISTORY`, `THREADLINE_STATUS`
- Trust + compute budget checks before forwarding messages
- In-memory or persistent (ContextThreadMap) thread mapping

---

## Relevance to Our Fleet

**Honest assessment: Architecturally interesting; largely superseded by what we already have.**

### What we already have (ClawBus)
Our fleet already has ClawBus — a working A2A message bus. Rocky built it, all
three agents are connected, it handles POST/GET/SSE. It's not as sophisticated as
Threadline (no crypto identity, no trust tiers, no circuit breaker) but it's live
and working today.

### What Threadline does better
| Feature | ClawBus | Threadline |
|---------|------------|------------|
| Delivery confirmation | ❌ | ✅ (ACK) |
| Agent identity | name string | Ed25519 keypair |
| Trust tiers | implicit (bearer token) | explicit 4-level system |
| Compute metering | ❌ | ✅ per agent/trust level |
| DNS discovery | ❌ (static config) | ✅ |
| Circuit breaker | ❌ | ✅ |
| MCP tool exposure | ❌ | ✅ |

### Key insight from OpenClawBridge
The bridge pattern itself is valuable — the idea of mapping our existing session
model to a more structured protocol without requiring a full rewrite. If we ever
upgrade ClawBus or adopt a proper A2A protocol, this bridge approach is the
right way to do it (thin adapter, inject callbacks, no hard dependencies on the
underlying protocol).

### What's directly stealable now
1. **Trust tier pattern** — even without the full Threadline stack, we could add
   `untrusted/basic/trusted/verified` tiers to ClawBus receiver logic. Useful
   if external agents (beyond Rocky/Bullwinkle/Boris) start connecting.

2. **Compute budget check pattern** — `computeMeter.check(agent, trustLevel, tokenEstimate)`
   before forwarding a message. Prevents a buggy/adversarial agent from burning
   our token budget.

3. **`DEFAULT_TOKEN_ESTIMATE = 500`** heuristic — good enough for budget checks
   when actual token count isn't known yet.

4. **ContextThreadMap concept** — persistent room→thread mapping survives restarts.
   We could adapt this for ClawBus to maintain conversation continuity across
   agent restarts.

### What's not worth porting now
- Full Threadline stack (Ed25519, DNS discovery, word-pairing bootstrap) — significant
  complexity for a 3-agent private fleet on Tailscale. Our threat model doesn't
  require crypto identity today.
- MCP tool exposure — potentially valuable long-term (agents exposing their tools
  to each other), but needs MCP server setup first.

---

## Recommendation

**Study only — note the patterns; no immediate adoption.**

Two specific pieces worth a future workqueue item:
1. **ClawBus trust tiers** — add `level` field to ClawBus sender identity,
   gate certain message types on trust level. Mostly relevant when external/untrusted
   agents connect.
2. **ClawBus delivery ACK** — the lack of delivery confirmation is a known gap
   (we even have an idea item for it: wq-API-1774346683592). Threadline's ACK pattern
   is the reference implementation to follow.

**Estimated effort for trust tiers on ClawBus:** ~3 hours.  
**Estimated effort for full Threadline adoption:** weeks; not worth it vs. ClawBus.

---

## Notes
- The `OpenClawBridge` note says OpenClaw is "formerly ElizaOS" — interesting historical
  context; the bridge predates OpenClaw's current architecture.
- `DEFAULT_TRUST_LEVEL: AgentTrustLevel = 'untrusted'` — good security default. We
  implicitly do this already (bearer tokens = trusted; no token = rejected) but making
  it explicit as tiers would be cleaner.
- The word-based pairing code bootstrap is genuinely clever for trusted-but-new agents.
  Worth considering if jkh ever wants to onboard a new agent without manual token
  distribution.
