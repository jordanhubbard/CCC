# RCC Codebase Audit
**Author:** Rocky  
**Date:** 2026-04-01  
**Scope:** Conformance to README design philosophy — "as much as necessary, as little as possible"

---

## TL;DR

RCC is **mostly healthy** — no external heavy deps, no reimplemented openclaw functionality, pure HTTP/stdlib Node.js. The main problems are: (1) `api/index.mjs` has metastasized to 8103 lines and contains multiple generations of code, (2) 58 env vars with no documented defaults or config consolidation, (3) duplicate routes for the same endpoint (GET + POST served from same path string), and (4) several agentOS-specific UI stubs that belong in agentOS, not RCC.

**Verdict: incremental cleanup, not green-field rewrite.** The architecture is sound; the implementation has accreted. Surgery, not amputation.

---

## Component Inventory

| Component | Lines | Status | Notes |
|-----------|-------|--------|-------|
| `api/index.mjs` | 8103 | ⚠️ Overgrown | All routes in one file — multiple generations visible |
| `brain/index.mjs` | 299 | ✅ Clean | LLM dispatch, tokenhub wired, good |
| `vector/index.mjs` | 680 | ✅ Clean | Milvus + tokenhub embeddings |
| `lessons/index.mjs` | 467 | ✅ Clean | Well-factored, does one thing |
| `issues/index.mjs` | 372 | ✅ Clean | GH issues cache |
| `scout/pump.mjs` | 336 | ✅ Clean | GitHub watcher |
| `llm/client.mjs` | 226 | ✅ Clean | PeerLLMClient, good fallback logic |
| `llm/registry.mjs` | 303 | ✅ Clean | LLM endpoint registry |
| `exec/agent-listener.mjs` | 332 | ✅ Clean | ClawBus exec listener |
| `crush-server/failover.mjs` | 321 | ✅ Clean | Failover logic |
| `guardrails/adaptive-trust.mjs` | 296 | ✅ Clean | Trust scoring |
| `decision-journal/intent-drift-detector.mjs` | 241 | ✅ Clean | Drift detection |
| `dashboard/geek-view-reference.mjs` | 2700 | ⚠️ Big | Reference for Leptos WASM; may be stale |

---

## Design Philosophy Conformance

### ✅ Things That Are Fine

1. **No external heavy deps** — pure Node.js `http`, `fs`, `crypto`. No express, no axios, no Slack SDK. Correct.
2. **Delegates LLM to tokenhub** — `brain/index.mjs` routes through tokenhub. ✅
3. **Delegates embeddings to tokenhub** — `vector/index.mjs` hits tokenhub for `text-embedding-3-large`. ✅
4. **No reimplemented messaging** — Slack messages go through openclaw's message tool, not a reimplemented SDK. ✅
5. **Secrets store** — `GET/POST /api/secrets/:key` is the right pattern. Agents use this instead of local env vars. ✅
6. **Heterogeneous hardware** — agents register on arrival, heartbeat, disappear gracefully. ✅
7. **Work queue** — clean JSON-file-backed queue, no DB required. Correct for the scale.

---

## Problems Found

### 🔴 P1: `api/index.mjs` is 8103 lines — multiple generations in one file

**Evidence:**
- Routes defined at lines 1941–3676 (public) and 3678–8103 (authed) — but some routes appear in BOTH sections (see duplicate routes below)
- `// ── Missing API endpoints (ported from old Node dashboard)` comment at line 6625 — this is legacy ported code that never got cleaned up
- agentOS-specific routes (`/api/agentos/*`, `/api/mesh`) are embedded in RCC's main API — they belong in agentOS
- Package registry browser UI HTML (lines ~1200–1900 and ~3500–3650 and ~7636–7849) appears **three times** — same HTML served from multiple routes

**Action:** Split into modules:
- `api/routes/queue.mjs` — queue + item CRUD
- `api/routes/agents.mjs` — agent registry + heartbeats
- `api/routes/agentos.mjs` — agentOS-specific routes (or delete if agentOS serves these itself)
- `api/routes/ui.mjs` — HTML page serving
- `api/index.mjs` — thin router that imports and mounts the above

---

### 🔴 P2: 58 env vars with no config consolidation

**Evidence:**
```
ACK_LOG_PATH, AGENTFS_URL, AGENTOS_ARCH, AGENTS_PATH, BOOTSTRAP_TOKENS_PATH,
BUS_LOG_PATH, CALENDAR_PATH, CAPABILITIES_PATH, CONVERSATIONS_PATH, DEAD_LOG_PATH,
DECISION_JOURNAL_PATH, DEFAULT_TRIAGING_AGENT, EXEC_LOG_PATH, HOME, LLM_REGISTRY_PATH,
MILVUS_URL, NANOC_BIN, OFFLINE_THRESHOLD_MS, OMGJKH_BOT, PROJECTS_PATH, PROVIDERS_PATH,
QUEUE_DEDUP_THRESHOLD, QUEUE_PATH, RCC_ADMIN_TOKEN, RCC_AGENT_TOKEN, RCC_AUTH_TOKENS,
RCC_EXTERNAL_URL, RCC_PORT, RCC_PUBLIC_URL, REPOS_PATH, REQUESTS_PATH, SBOM_DIR,
SECRETS_PATH, SLACK_AGENT_CHANNEL, SLACK_BOT_TOKEN, SLACK_NOTIFY_USER, SLACK_SIGNING_SECRET,
SPARKY_OLLAMA_URL, SQUIRRELBUS_TOKEN, SQUIRRELBUS_URL, STALE_CLAUDE_MS, STALE_DEFAULT_MS,
STALE_GPU_MS, STALE_INFERENCE_MS, STALE_LLM_MS, TOKENHUB_ADMIN_TOKEN, TOKENHUB_URL,
TUNNEL_AUTH_KEYS, TUNNEL_PORT_START, TUNNEL_STATE_PATH, TUNNEL_USER, USERS_PATH,
WORKSPACE_DIR
```

Most of these are just path overrides with sensible defaults. They don't need to be env vars — they should be in `rcc.json` with `~/.rcc/rcc.json` as the config file.

**Action:** This is the first-run setup wizard work (`wq-API-1775019456431`). Move non-secret config to `rcc.json`, keep secrets in `data/secrets.json`.

---

### 🔴 P3: Duplicate route definitions

The following endpoints are defined **twice** in `api/index.mjs` — once in the public section and once in the authed section:

```
/api/bootstrap, /api/brain/status, /api/calendar, /api/conversations,
/api/cron-status, /api/keys/github, /api/llms, /api/onboard, /api/projects,
/api/provider-health, /api/providers, /api/queue, /api/requests, /api/sbom,
/api/secrets, /api/users
```

The second definition shadows the first (whichever matches first wins in the linear scan). This is dead code at best, a security issue at worst (public route being shadowed by auth-required route, or vice versa).

**Action:** Audit each duplicate pair. Delete the stale one. Add a test that each route appears exactly once.

---

### 🟡 P4: agentOS routes in RCC (`/api/agentos/*`, `/api/mesh`)

Lines 6627–7082 and 7849–8103 implement agentOS-specific routes:
- WASM slot health, debug bridge, breakpoints, step, console_mux, dev_shell, cap-events, mesh topology, agentOS timeline

These are only relevant when an agentOS instance is running. RCC shouldn't own them — agentOS should proxy/serve them, or they should live in a separate `agentos-bridge` service.

**Signs of abandonment:** Several have `// TODO: wire QEMU pipe` comments and return stub data.

**Action:** Move to `agentOS` repo or a separate `rcc/agentos-bridge/` module. Not RCC's core concern.

---

### 🟡 P5: Package registry HTML rendered three times

The nanolang package registry browser UI HTML appears at:
- Lines ~1200–1900 (inline in a GET handler)  
- Lines ~3452–3580 (second GET `/pkg` handler)
- Lines ~7636–7849 (third occurrence, commented "ported from old Node dashboard")

Only one of these is the canonical one. The others are dead.

**Action:** Keep one, delete the other two, extract the HTML to `api/templates/pkg-browser.html`.

---

### 🟡 P6: `SPARKY_OLLAMA_URL` — hardcoded agent reference

`SPARKY_OLLAMA_URL` is a specific agent's URL baked into RCC config. This defeats the purpose of the LLM registry (`/api/llms`). Sparky should register its endpoint via the registry like everyone else.

**Action:** Remove `SPARKY_OLLAMA_URL`. If anything still uses it, replace with a `llmRegistry.getBest({ tag: 'ollama' })` call.

---

### 🟡 P7: `OMGJKH_BOT` env var — operator-specific config in a generic tool

RCC bills itself as a generic multi-agent framework, but `OMGJKH_BOT` is a specific Slack bot token for jkh's workspace. This leaks operator-specific config into the core.

**Action:** Move to `data/secrets.json` under a generic key like `slack/bot_token` (already supported by the secrets API).

---

### 🟢 P8 (cosmetic): `crush-server/` directory name

"Crush server" is an unusual name. Based on `failover.mjs`, it appears to be a failover/HA coordinator. The name doesn't communicate its purpose.

**Action (optional):** Rename to `failover/` or `ha/`. Low priority.

---

## Cross-reference: Reform Tasks vs Root Causes

The 10-30 "reform" tasks filed recently appear to be patching symptoms of P2 (env var sprawl → setup wizard) and P1 (route sprawl → split api/index.mjs). They're not wrong to fix, but fixing P1 and P2 will make most of them obsolete or trivial.

**Recommended order:**
1. Fix P3 (duplicate routes) — quick, mechanical, reduces security risk
2. Fix P2 (config consolidation) — this IS the setup wizard task, already claimed
3. Fix P1 (split api/index.mjs) — biggest leverage, unblocks all reform tasks
4. Fix P4 (agentOS routes) — after agentOS has its own API surface
5. Fix P5 (pkg registry dedup) — quick, cosmetic
6. Fix P6+P7 (hardcoded agent refs) — quick

---

## Recommendation

**Incremental cleanup.** The architecture is correct — pure HTTP, single responsibility per module (outside api/index.mjs), good delegation to tokenhub/openclaw. The main failure mode was growing api/index.mjs as a monolith instead of splitting routes into modules.

A green-field rewrite would reproduce the same issues unless the split-by-module discipline is enforced from day one. Better to split the existing code.

**Estimated effort:**
- P3 (duplicate routes): 2-4 hours
- P2 (config consolidation / setup wizard): 1-2 days (already claimed as separate task)
- P1 (split api/index.mjs into route modules): 1 day
- P4 (agentOS routes): 0.5 days (move, don't rewrite)
- P5+P6+P7: 1-2 hours total

Total: ~3-4 days of focused work to get RCC to the "clean" state the README implies.
