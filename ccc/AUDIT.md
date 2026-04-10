# CCC Codebase Audit
*Conducted by Natasha, 2026-04-01. Benchmark: README.md design philosophy — "as much as necessary, as little as possible." Delegate to openclaw, tokenhub, linux itself.*

**Supplements:** [`SOA-AUDIT-2026-04-02.md`](SOA-AUDIT-2026-04-02.md) (production topology, Rust migration, deletion list). **Closure:** [§ Audit closure (2026-04-07)](#audit-closure-2026-04-07) finishes `wq-RCC-audit-001` deliverables against the CCC tree.

---

## TL;DR

The core of CCC is a single 8,176-line Node.js file (`ccc/api/index.mjs`) that has grown by accretion over multiple generations and now contains the REST API, auth, ClawBus, brain orchestration, Slack integration, tunnel management, inline HTML/JS for six different UIs (~1,220 lines), and two duplicated function definitions. The module directory structure (`brain/`, `scout/`, `vector/`, etc.) exists and has working code, but most of it is imported into the monolith rather than running as independent services. The design philosophy says "delegate to openclaw, tokenhub, linux" — in practice, CCC has re-implemented Slack messaging, LLM routing, and HTML-serving that already exists in those systems.

RCC/CCC is **mostly healthy** — no external heavy deps, no reimplemented openclaw functionality, pure HTTP/stdlib Node.js. The main problems are: (1) `api/index.mjs` has metastasized to 8103+ lines and contains multiple generations of code, (2) 58 env vars with no documented defaults or config consolidation, (3) duplicate routes for the same endpoint (GET + POST served from same path string), and (4) several agentOS-specific UI stubs that belong in agentOS, not CCC.

**Verdict: incremental cleanup, not green-field rewrite.** The architecture is sound; the implementation has accreted. Surgery, not amputation. **Recommendation: targeted refactor, not greenfield.** The core data model (queue, agents, heartbeats, secrets) is sound. The problem is surface area — too much code doing things it shouldn't own.

---

## Component Inventory

### Detailed line-by-line breakdown (`api/index.mjs`)

| Section | Lines | What it does | Verdict | Notes |
|---------|-------|--------------|---------|-------|
| Config / env vars | 27–51 | 50+ process.env reads, path constants | **Extract** | Should be ccc.json + config module. wq-CCC-setup-001 covers this. |
| Services map | 52–128 | Probes URLs, returns health | **Keep / Extract** | Move to `ccc/services/index.mjs` |
| Semantic dedup | 129–172 | Background Milvus indexer for queue dedup | **Keep / Extract** | Move to `ccc/vector/` (module already exists there) |
| ClawBus | 173–238 | File-based message bus (JSONL) | **Extract** | `clawbus/` dir exists at repo root with a separate implementation. Duplication — needs reconciliation. |
| Slack config + helpers | 239–285, 1802–1900 | Bot token, signing secret, post/verify/format | **Delegate** | openclaw handles Slack natively. CCC should send via openclaw or tokenhub, not own a bot. |
| Heartbeats | 286–352 | In-memory agent heartbeat tracking, offline detection | **Keep / Extract** | Core CCC feature. Move to `ccc/agents/heartbeat.mjs` |
| Disappearance detection | 353–403 | Polls agents, fires Slack alert on offline | **Keep, simplify** | The Slack alert should go through openclaw, not a direct bot call |
| JSON file helpers | 404–636 | readJsonFile, writeJsonFile, queue/agents/secrets I/O | **Keep / Extract** | Move to `ccc/lib/storage.mjs` |
| HTML: dashboardHtml | 725–1157 | 432-line inline HTML dashboard | **Delete** | Superseded by Leptos WASM dashboard in `ccc/dashboard/`. Dead code. |
| HTML: projectsListHtml | 1158–1186 | Inline HTML projects list | **Delete** | Superseded by dashboard. |
| HTML: packagesHtml | 1187–1345 | Inline HTML package registry (158 lines) | **Review** | Is this still used? If wasm-dashboard replaced it, delete. |
| HTML: playgroundHtml | 1346–1540 | nanolang browser playground (194 lines) | **Review** | Check if live on do-host1 and used. If so, extract to standalone file. |
| HTML: timelineHtml | 1541–1619 | Inline timeline UI (78 lines) | **Review** | Same — check if wasm dashboard replaced it. |
| HTML: servicesHtml | 1620–1741 | Inline services health UI (121 lines) | **Review** | Same. |
| HTML: projectDetailHtml | 1742–1801 | Inline project detail (59 lines) | **Review** | Same. |
| Brain lazy init | 474–494 | Imports Brain from brain/index.mjs, lazy-init | **Keep** | Brain module exists separately (299 lines), correctly imported. Good. |
| Pump lazy init | 485–494 | Work pump init | **Keep / Extract** | Tightly coupled to brain. Fine for now. |
| Auth | 495–509 | isAuthed(), isAdminAuthed() — token check | **Keep, expand** | Currently just Bearer token. wq-SC-auth-001 adds user auth on top. |
| handleRequest | 1902–8076 | Giant router switch — ~6,000 lines of route handlers | **Extract** | This IS the problem. Should be split into route modules. |
| Inline JS in HTML | ~3512–3554, ~7696–7738 | `loadPackages()`, `render()`, `esc()` duplicated twice | **Delete** | Literal copy-paste duplication. One copy is dead code. |
| startServer | 8078–8176 | HTTP server init, token reload, LLM registry, stale expiry | **Keep / Extract** | Startup logic fine. LLM registry init could move to tokenhub module. |

### Module health summary

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

### ⚠️ Things That Are Wrong

- **Inline HTML UIs (~1,220 lines total):** `dashboardHtml()`, `projectsListHtml()`, `packagesHtml()`, `playgroundHtml()`, `timelineHtml()`, `servicesHtml()`, `projectDetailHtml()` — all predate the Leptos WASM dashboard. At minimum the dashboard and projects HTML are dead. The others need confirmation before deletion.
- **Duplicated functions:** `loadPackages()`, `render()`, `esc()` appear at lines 3512–3554 AND 7696–7738. One copy is dead code from a copy-paste that never got cleaned up.
- **`ccc/wasm-dashboard/`:** A separate, older Rust/Leptos dashboard attempt (19 files, Cargo.toml, Trunk.toml). The current dashboard is in `ccc/dashboard/`. `wasm-dashboard/` appears to be a dead predecessor — **candidate for deletion, verify first.**
- **`ccc/crush-server/`:** 630 files (mostly node_modules). This is the "crush" coding agent integration that was disabled in `app.rs` comments (`// coding_agent::CodingAgent, // disabled — crush-server not deployed`). May be entirely superseded by the ACP/Codex delegation pattern. **Candidate for deletion, verify first.**
- **`ccc/heartbeat-local/`:** 3 files, separate `server.mjs`. Appears to be an early standalone heartbeat server before it was folded into the main API. Likely dead.
- **`ccc/ideation/`:** 1 file. Unclear if active.
- **ClawBus duplication:** `clawbus/` at repo root has `receive-server.mjs`, `context-handoff.mjs`, `SPEC.md`. `ccc/api/index.mjs` has its own in-process bus at lines 173–238. These are two different implementations. The standalone `clawbus/` appears to be the original design; the in-process version is what's actually running. **Needs reconciliation.**

---

## Problems Found

### Delegation gaps

| Feature | Currently in | Should delegate to | Reasoning |
|---------|-------------|-------------------|-----------|
| Slack messaging | `ccc/api/index.mjs` (~200 lines) | openclaw | openclaw has a full Slack plugin. CCC agents already receive Slack via openclaw. Having CCC maintain its own bot duplicates infrastructure and credentials. |
| LLM calls | scattered in api/index.mjs | tokenhub | tokenhub is the LLM proxy. All LLM calls in CCC should go through `localhost:8090/v1/` — some do, some don't. |
| HTML serving | inline in api/index.mjs | dashboard-ui (Leptos) or standalone HTML files | The API should return JSON. HTML belongs in the dashboard or static files served by dashboard-server. |
| SSH tunnel management | api/index.mjs tunnel routes | linux (systemd SSH tunnel services) | Tunnel lifecycle is better managed by systemd. CCC can read status but shouldn't own the lifecycle. |
| SMTP/email | missing | nodemailer + Resend (wq-SC-auth-001) | Email sending is a utility, not a core CCC concern. Wire once, delegate everywhere. |
| Config management | 50+ process.env vars | ccc.json + setup TUI (wq-CCC-setup-001) | linux env vars are fine for secrets; a config file is better for structured settings. |

---

### 🔴 P1: `api/index.mjs` is 8103+ lines — multiple generations in one file

**Evidence:**
- Routes defined at lines 1941–3676 (public) and 3678–8103 (authed) — but some routes appear in BOTH sections (see duplicate routes below)
- `// ── Missing API endpoints (ported from old Node dashboard)` comment at line 6625 — this is legacy ported code that never got cleaned up
- agentOS-specific routes (`/api/agentos/*`, `/api/mesh`) are embedded in CCC's main API — they belong in agentOS
- Package registry browser UI HTML (lines ~1200–1900 and ~3500–3650 and ~7636–7849) appears **three times** — same HTML served from multiple routes

**Action:** Split into modules:
- `api/routes/queue.mjs` — queue + item CRUD
- `api/routes/agents.mjs` — agent registry + heartbeats
- `api/routes/agentos.mjs` — agentOS-specific routes (or delete if agentOS serves these itself)
- `api/routes/ui.mjs` — HTML page serving
- `api/index.mjs` — thin router that imports and mounts the above

**Target directory structure:**
```
ccc/
  api/
    index.mjs          ← slim: imports routes, starts server (~200 lines target)
    routes/
      queue.mjs        ← all /api/queue/* handlers
      agents.mjs       ← all /api/agents/*, /api/heartbeat/* handlers
      projects.mjs     ← all /api/projects/* handlers
      secrets.mjs      ← /api/secrets/* handlers
      exec.mjs         ← /api/exec/* handlers
      bus.mjs          ← /bus/* handlers (or delegate to clawbus/)
      github.mjs       ← /api/github/* handlers (or delegate to ccc/scout/)
      slack.mjs        ← /api/slack/* handlers (or delete if delegating to openclaw)
      users.mjs        ← /api/users/*, /api/auth/* handlers (wq-SC-auth-001)
      setup.mjs        ← /api/setup/* handlers (wq-CCC-setup-001)
  lib/
    storage.mjs        ← readJsonFile, writeJsonFile, all I/O helpers
    auth.mjs           ← isAuthed, isAdminAuthed, token management
    config.mjs         ← all process.env reads, ccc.json loading
    slack.mjs          ← slackPost, verifySlackSignature (or delete)
```

**Migration strategy:** Extract one route group at a time. Each extraction is a separate commit. Tests exist in `ccc/api/test.mjs` — run after each extraction to confirm nothing broke.

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

Most of these are just path overrides with sensible defaults. They don't need to be env vars — they should be in `ccc.json` with `~/.ccc/ccc.json` as the config file.

**Mandatory (server won't work without these):**
```
CCC_PORT / RCC_PORT       Server port (default 8789)
CCC_PUBLIC_URL            Public URL for callbacks and links
CCC_ADMIN_TOKEN           Admin auth token
CCC_AUTH_TOKENS           Comma-separated agent tokens
```

**Path overrides (have working defaults):**
```
QUEUE_PATH, AGENTS_PATH, SECRETS_PATH, PROJECTS_PATH, REPOS_PATH,
CALENDAR_PATH, REQUESTS_PATH, CAPABILITIES_PATH, EXEC_LOG_PATH,
BUS_LOG_PATH, ACK_LOG_PATH, DEAD_LOG_PATH, BOOTSTRAP_TOKENS_PATH,
DECISION_JOURNAL_PATH, LLM_REGISTRY_PATH, PROVIDERS_PATH,
CONVERSATIONS_PATH, USERS_PATH, TUNNEL_STATE_PATH, SBOM_DIR
```

**Tuning (safe defaults):**
```
STALE_CLAUDE_MS, STALE_GPU_MS, STALE_INFERENCE_MS, STALE_LLM_MS,
STALE_DEFAULT_MS, OFFLINE_THRESHOLD_MS, QUEUE_DEDUP_THRESHOLD,
TUNNEL_PORT_START
```

**Unclear / possibly legacy:**
```
OMGJKH_BOT              Alias for SLACK_BOT_TOKEN? Or separate bot?
AGENTFS_URL             agentOS filesystem — active or aspirational?
AGENTOS_ARCH            agentOS architecture flag
WORKSPACE_DIR           OpenClaw workspace path — why is CCC reading this?
DEFAULT_TRIAGING_AGENT  Brain routing hint — check if still used
NANOC_BIN               nanolang compiler path — only used in playground
```

**Action:** This is the first-run setup wizard work (`wq-CCC-setup-001` / `wq-API-1775019456431`). Move non-secret config to `ccc.json`, keep secrets in `data/secrets.json`.

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

**Workqueue files status:**

| File | Status | Notes |
|------|--------|-------|
| wq-CCC-setup-001.json | **Active** (proposed) | First-run TUI + config management. Prerequisite for wq-CCC-audit-001 action items. |
| wq-CCC-audit-001.json | **Active** (this audit) | Produces this document. |
| queue.json.bak-20260329 | **Safe to delete** | Backup from March 29. Production queue has moved on. |
| queue.json.bak-20260331-073218 | **Safe to delete** | Backup from March 31 morning. |

**Action:** Audit each duplicate pair. Delete the stale one. Add a test that each route appears exactly once.

---

### 🟡 P4: agentOS routes in CCC (`/api/agentos/*`, `/api/mesh`)

Lines 6627–7082 and 7849–8103 implement agentOS-specific routes:
- WASM slot health, debug bridge, breakpoints, step, console_mux, dev_shell, cap-events, mesh topology, agentOS timeline

These are only relevant when an agentOS instance is running. CCC shouldn't own them — agentOS should proxy/serve them, or they should live in a separate `agentos-bridge` service.

**Signs of abandonment:** Several have `// TODO: wire QEMU pipe` comments and return stub data.

**High confidence — safe to remove (verify first):**
- `ccc/wasm-dashboard/` — superseded by `ccc/dashboard/`
- `ccc/heartbeat-local/` — superseded by heartbeat in api/index.mjs
- Duplicate `loadPackages()` / `render()` / `esc()` at lines 7696–7738 in api/index.mjs

**Likely dead — needs confirmation before removing:**
- `ccc/crush-server/` — disabled in dashboard, 630 files mostly node_modules
- Inline HTML functions: `dashboardHtml()`, `projectsListHtml()` — superseded by Leptos WASM
- `ccc/ideation/` — 1 file, unclear if active
- `workqueue/queue.json.bak-*` — backup files

**Do not delete without full crew review:**
- `ccc/patches/` — may contain important migration patches
- `ccc/sbom/` — software bill of materials, may be needed for compliance
- `ccc/trust/`, `ccc/guardrails/`, `ccc/hooks/` — small but potentially wired into production

**Action:** Move to `agentOS` repo or a separate `ccc/agentos-bridge/` module. Not CCC's core concern.

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

`SPARKY_OLLAMA_URL` is a specific agent's URL baked into CCC config. This defeats the purpose of the LLM registry (`/api/llms`). Sparky should register its endpoint via the registry like everyone else.

**Action:** Remove `SPARKY_OLLAMA_URL`. If anything still uses it, replace with a `llmRegistry.getBest({ tag: 'ollama' })` call.

---

### 🟡 P7: `OMGJKH_BOT` env var — operator-specific config in a generic tool

CCC bills itself as a generic multi-agent framework, but `OMGJKH_BOT` is a specific Slack bot token for jkh's workspace. This leaks operator-specific config into the core.

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

**Phased action plan:**

**Phase 1 — Quick wins (no production risk):**
1. Delete `queue.json.bak-*` files
2. Delete duplicate `loadPackages()`/`render()`/`esc()` at lines 7696–7738 in api/index.mjs
3. Delete `ccc/wasm-dashboard/` (confirm first)
4. Delete `ccc/heartbeat-local/` (confirm first)
5. Confirm and delete `ccc/crush-server/` if disabled (confirm — 630 files)

**Phase 2 — Config consolidation (prerequisite for everything else):**
6. Implement `ccc.json` config file + `ccc/lib/config.mjs` loader (wq-CCC-setup-001)
7. Move all mandatory + high-value env vars to ccc.json with env fallback
8. Document which `OMGJKH_BOT` / `AGENTFS_URL` / `WORKSPACE_DIR` vars are legacy

**Phase 3 — Route extraction (breaks the monolith):**
9. Extract `ccc/api/routes/queue.mjs` — the queue routes are the most self-contained
10. Extract `ccc/api/routes/agents.mjs` — heartbeat + registry
11. Extract `ccc/lib/storage.mjs` — all JSON I/O helpers
12. Extract remaining route groups one at a time, test after each

**Phase 4 — Delegation (shrinks CCC's surface area):**
13. Evaluate delegating Slack notifications to openclaw (removes ~200 lines + credentials)
14. Audit all LLM calls in api/index.mjs — ensure all go through tokenhub
15. Reconcile ClawBus: pick one implementation (in-process vs standalone), delete the other

**Phase 5 — HTML cleanup (cosmetic but meaningful):**
16. Confirm which inline HTML functions are still served in production
17. Delete dead HTML functions (dashboardHtml at minimum — superseded by WASM dashboard)
18. Any surviving HTML pages → extract to `ccc/api/static/` as standalone files

**Estimated effort:**
- P3 (duplicate routes): 2-4 hours
- P2 (config consolidation / setup wizard): 1-2 days (already claimed as separate task)
- P1 (split api/index.mjs into route modules): 1 day
- P4 (agentOS routes): 0.5 days (move, don't rewrite)
- P5+P6+P7: 1-2 hours total

Total: ~3-4 days of focused work to get CCC to the "clean" state the README implies.

---

## Audit closure (2026-04-07)

This section completes **`workqueue/wq-RCC-audit-001.json`** against the **CCC** repository state. The narrative and line numbers above are a **2026-04-01 baseline**; several items have since moved or shrunk.

### Revised metrics (CCC tree)

| Area | Approx. lines | Notes |
|------|----------------|-------|
| `api/index.mjs` | ~2132 | HTTP server, shared state, Slack/catalog helpers, router wiring — no longer an ~8k monolith |
| `api/routes/*.mjs` | ~5000+ combined | `queue`, `agents`, `bus`, `agentos`, `memory`, `ui`, `services`, `projects`, `setup` |
| `dashboard/geek-view-reference.mjs` | ~2700 | Unchanged — still "big reference" risk |

**P1 (split API):** Largely addressed by route modules. Remaining work is shrinking `index.mjs` further (and/or **Rust parity** per `SOA-AUDIT-2026-04-02.md` and `wq-RCC-rust-migration-001`).

**P3 (duplicate routes):** The old "public block vs authed block" pattern in one file is **replaced** by `createRouter()` + ordered `register*` calls. One duplicate remained: **`GET /api/queue/activity-feed`** was registered in both `routes/services.mjs` and `routes/queue.mjs`; the router uses **first match**, so the `services` copy shadowed `queue`. The extra registration was **removed** from `services.mjs` (2026-04-07) so `queue.mjs` is canonical.

**P2 / P6 / P7:** Still open — `SPARKY_OLLAMA_URL` and `OMGJKH_BOT` still appear in `api/index.mjs`, `api/routes/queue.mjs`, and scripts. Config consolidation remains **`wq-RCC-setup-001`** / `wq-API-1775019456431` territory.

### Delegation map (condensed)

| Capability | Keep in CCC | Delegate / move |
|------------|-------------|-----------------|
| Queue, agents, heartbeats, secrets, bus | Yes (core) | — |
| LLM calls | Thin client | **tokenhub** |
| Slack UX | Minimal bridge if any | **openclaw** |
| Embeddings / Milvus | Orchestration | **tokenhub** + Milvus API |
| agentOS debug / mesh | Until bridge exists | **agentOS** or `agentos-bridge` (P4) |
| HTML playground / pkg pages | Optional | **dashboard-server** + WASM UI |

### Workqueue reconciliation

| Workqueue id | Role vs this audit |
|--------------|-------------------|
| `wq-RCC-audit-001` | **Closed** — report + deliverables anchored here and in SOA audit |
| `wq-RCC-setup-001` | First-run TUI + config; implements P2-style consolidation |
| `wq-RCC-rust-migration-001` | Node → Rust; subsumes long-term P1 |
| `wq-SC-auth-001` | Users/bootstrap/auth surface |

### Proposed `ccc.json` shape (non-secrets only)

Illustrative schema for moving path/port/integration defaults out of raw `process.env` (secrets stay in `data/secrets.json` / `/api/secrets`):

```json
{
  "version": 1,
  "server": { "port": 8789, "publicUrl": "http://localhost:8789" },
  "paths": {
    "queue": "../../workqueue/queue.json",
    "agents": "./agents.json",
    "secrets": "../data/secrets.json",
    "users": "./data/users.json",
    "calendar": "./data/calendar.json",
    "conversations": "./data/conversations.json",
    "llmRegistry": "./data/llm-registry.json",
    "providers": "./data/providers.json",
    "tunnelState": "./data/tunnel-state.json",
    "sbomDir": "./sbom"
  },
  "tunnel": { "user": "tunnel", "portStart": 18080, "authKeys": "/home/tunnel/.ssh/authorized_keys" },
  "staleClaims": { "defaultMs": null, "claudeMs": null, "gpuMs": null, "llmMs": null, "inferenceMs": null },
  "integrations": {
    "tokenhubUrl": "http://127.0.0.1:8090",
    "milvusUrl": "http://127.0.0.1:9091",
    "clawbusUrl": ""
  }
}
```

`null` in `staleClaims` means "use built-in defaults / env overrides" until the setup wizard writes real values.

### Deletion candidates (verify before rm)

From SOA audit — **confirm paths exist** in this clone: `rcc-hub/`, `rcc/wasm-dashboard/`, `workqueue/queue.json.bak-*`, `rcc/heartbeat-local/`. Some items (e.g. Node `squirrelchat/`) may not be present in CCC; treat the SOA list as a checklist, not a script.
