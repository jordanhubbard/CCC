# RCC Codebase Audit
*Conducted by Natasha, 2026-04-01. Benchmark: README.md design philosophy — "as much as necessary, as little as possible." Delegate to openclaw, tokenhub, linux itself.*

---

## Executive Summary

The core of RCC is a single 8,176-line Node.js file (`rcc/api/index.mjs`) that has grown by accretion over multiple generations and now contains the REST API, auth, ClawBus, brain orchestration, Slack integration, tunnel management, inline HTML/JS for six different UIs (~1,220 lines), and two duplicated function definitions. The module directory structure (`brain/`, `scout/`, `vector/`, etc.) exists and has working code, but most of it is imported into the monolith rather than running as independent services. The design philosophy says "delegate to openclaw, tokenhub, linux" — in practice, RCC has re-implemented Slack messaging, LLM routing, and HTML-serving that already exists in those systems. **Recommendation: targeted refactor, not greenfield.** The core data model (queue, agents, heartbeats, secrets) is sound. The problem is surface area — too much code doing things it shouldn't own.

---

## api/index.mjs — Section Map

| Section | Lines | What it does | Verdict | Notes |
|---------|-------|--------------|---------|-------|
| Config / env vars | 27–51 | 50+ process.env reads, path constants | **Extract** | Should be rcc.json + config module. wq-RCC-setup-001 covers this. |
| Services map | 52–128 | Probes URLs, returns health | **Keep / Extract** | Move to `rcc/services/index.mjs` |
| Semantic dedup | 129–172 | Background Milvus indexer for queue dedup | **Keep / Extract** | Move to `rcc/vector/` (module already exists there) |
| ClawBus | 173–238 | File-based message bus (JSONL) | **Extract** | `squirrelbus/` dir exists at repo root with a separate implementation. Duplication — needs reconciliation. |
| Slack config + helpers | 239–285, 1802–1900 | Bot token, signing secret, post/verify/format | **Delegate** | openclaw handles Slack natively. RCC should send via openclaw or tokenhub, not own a bot. |
| Heartbeats | 286–352 | In-memory agent heartbeat tracking, offline detection | **Keep / Extract** | Core RCC feature. Move to `rcc/agents/heartbeat.mjs` |
| Disappearance detection | 353–403 | Polls agents, fires Slack alert on offline | **Keep, simplify** | The Slack alert should go through openclaw, not a direct bot call |
| JSON file helpers | 404–636 | readJsonFile, writeJsonFile, queue/agents/secrets I/O | **Keep / Extract** | Move to `rcc/lib/storage.mjs` |
| HTML: dashboardHtml | 725–1157 | 432-line inline HTML dashboard | **Delete** | Superseded by Leptos WASM dashboard in `rcc/dashboard/`. Dead code. |
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

---

## Legacy Code Findings

- **Inline HTML UIs (~1,220 lines total):** `dashboardHtml()`, `projectsListHtml()`, `packagesHtml()`, `playgroundHtml()`, `timelineHtml()`, `servicesHtml()`, `projectDetailHtml()` — all predate the Leptos WASM dashboard. At minimum the dashboard and projects HTML are dead. The others need confirmation from Rocky before deletion.
- **Duplicated functions:** `loadPackages()`, `render()`, `esc()` appear at lines 3512–3554 AND 7696–7738. One copy is dead code from a copy-paste that never got cleaned up.
- **rcc/wasm-dashboard/:** A separate, older Rust/Leptos dashboard attempt (19 files, Cargo.toml, Trunk.toml). The current dashboard is in `rcc/dashboard/`. `wasm-dashboard/` appears to be a dead predecessor — **candidate for deletion, verify with Rocky.**
- **rcc/crush-server/:** 630 files (mostly node_modules). This is the "crush" coding agent integration that was disabled in `app.rs` comments (`// coding_agent::CodingAgent, // disabled — crush-server not deployed`). May be entirely superseded by the ACP/Codex delegation pattern. **Candidate for deletion, verify with Rocky.**
- **rcc/heartbeat-local/:** 3 files, separate `server.mjs`. Appears to be an early standalone heartbeat server before it was folded into the main API. Likely dead.
- **rcc/ideation/:** 1 file. Unclear if active.
- **ClawBus duplication:** `squirrelbus/` at repo root has `receive-server.mjs`, `context-handoff.mjs`, `SPEC.md`. `rcc/api/index.mjs` has its own in-process bus at lines 173–238. These are two different implementations. The standalone `squirrelbus/` appears to be the original design; the in-process version is what's actually running. **Needs reconciliation.**

---

## Delegation Candidates

| Feature | Currently in | Should delegate to | Reasoning |
|---------|-------------|-------------------|-----------|
| Slack messaging | rcc/api/index.mjs (~200 lines) | openclaw | openclaw has a full Slack plugin. RCC agents already receive Slack via openclaw. Having RCC maintain its own bot duplicates infrastructure and credentials. |
| LLM calls | scattered in api/index.mjs | tokenhub | tokenhub is the LLM proxy. All LLM calls in RCC should go through `localhost:8090/v1/` — some do, some don't. |
| HTML serving | inline in api/index.mjs | dashboard-ui (Leptos) or standalone HTML files | The API should return JSON. HTML belongs in the dashboard or static files served by dashboard-server. |
| SSH tunnel management | api/index.mjs tunnel routes | linux (systemd SSH tunnel services) | Tunnel lifecycle is better managed by systemd. RCC can read status but shouldn't own the lifecycle. |
| SMTP/email | missing | nodemailer + Resend (wq-SC-auth-001) | Email sending is a utility, not a core RCC concern. Wire once, delegate everywhere. |
| Config management | 50+ process.env vars | rcc.json + setup TUI (wq-RCC-setup-001) | linux env vars are fine for secrets; a config file is better for structured settings. |

---

## Module Decomposition Plan

Breaking `api/index.mjs` (8,176 lines) into focused modules without breaking production:

```
rcc/
  api/
    index.mjs          ← slim: imports routes, starts server (~200 lines target)
    routes/
      queue.mjs        ← all /api/queue/* handlers
      agents.mjs       ← all /api/agents/*, /api/heartbeat/* handlers
      projects.mjs     ← all /api/projects/* handlers
      secrets.mjs      ← /api/secrets/* handlers
      exec.mjs         ← /api/exec/* handlers
      bus.mjs          ← /bus/* handlers (or delegate to squirrelbus/)
      github.mjs       ← /api/github/* handlers (or delegate to rcc/scout/)
      slack.mjs        ← /api/slack/* handlers (or delete if delegating to openclaw)
      users.mjs        ← /api/users/*, /api/auth/* handlers (wq-SC-auth-001)
      setup.mjs        ← /api/setup/* handlers (wq-RCC-setup-001)
  lib/
    storage.mjs        ← readJsonFile, writeJsonFile, all I/O helpers
    auth.mjs           ← isAuthed, isAdminAuthed, token management
    config.mjs         ← all process.env reads, rcc.json loading
    slack.mjs          ← slackPost, verifySlackSignature (or delete)
```

**Migration strategy:** Extract one route group at a time. Each extraction is a separate commit. Tests exist in `rcc/api/test.mjs` — run after each extraction to confirm nothing broke.

---

## Workqueue Reconciliation

| File | Status | Notes |
|------|--------|-------|
| wq-SC-auth-001.json | **Active** (in-progress) | ClawChat auth system. Backend shipped in f7075c3. Email provider still pending. |
| wq-RCC-setup-001.json | **Active** (proposed) | First-run TUI + config management. Prerequisite for wq-RCC-audit-001 action items. |
| wq-RCC-audit-001.json | **Active** (this audit) | Produces this document. |
| queue.json.bak-20260329 | **Safe to delete** | Backup from March 29. Production queue has moved on. |
| queue.json.bak-20260331-073218 | **Safe to delete** | Backup from March 31 morning. |

**Note:** Rocky filed additional tasks in the queue.json workqueue (IDs like `wq-API-*`) that are not reflected as standalone JSON files. Those should be reviewed in `queue.json` directly and reconciled with the JSON files above.

---

## Dead Code / Dead Directories

**High confidence — safe to remove (verify with Rocky first):**
- `rcc/wasm-dashboard/` — superseded by `rcc/dashboard/`
- `rcc/heartbeat-local/` — superseded by heartbeat in api/index.mjs
- Duplicate `loadPackages()` / `render()` / `esc()` at lines 7696–7738 in api/index.mjs

**Likely dead — needs Rocky confirmation before removing:**
- `rcc/crush-server/` — disabled in dashboard, 630 files mostly node_modules
- Inline HTML functions: `dashboardHtml()`, `projectsListHtml()` — superseded by Leptos WASM
- `rcc/ideation/` — 1 file, unclear if active
- `workqueue/queue.json.bak-*` — backup files

**Do not delete without full crew review:**
- `rcc/patches/` — may contain important migration patches
- `rcc/sbom/` — software bill of materials, may be needed for compliance
- `rcc/trust/`, `rcc/guardrails/`, `rcc/hooks/` — small but potentially wired into production

---

## Config Scatter

**Mandatory (server won't work without these):**
```
RCC_PORT                  Server port (default 8789)
RCC_PUBLIC_URL            Public URL for callbacks and links
RCC_ADMIN_TOKEN           Admin auth token
RCC_AUTH_TOKENS           Comma-separated agent tokens
```

**High value (features degrade without these):**
```
SLACK_BOT_TOKEN           Slack integration
SLACK_SIGNING_SECRET      Slack webhook verification
TOKENHUB_URL              LLM proxy
TOKENHUB_ADMIN_TOKEN      TokenHub management
SQUIRRELBUS_URL           Agent-to-agent bus
SQUIRRELBUS_TOKEN         Bus auth
MILVUS_URL                Vector store (semantic dedup)
SPARKY_OLLAMA_URL         Local Ollama endpoint
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
WORKSPACE_DIR           OpenClaw workspace path — why is RCC reading this?
DEFAULT_TRIAGING_AGENT  Brain routing hint — check if still used
NANOC_BIN               nanolang compiler path — only used in playground
```

---

## Recommended Action Order

**Phase 1 — Quick wins (no production risk):**
1. Delete `queue.json.bak-*` files
2. Delete duplicate `loadPackages()`/`render()`/`esc()` at lines 7696–7738 in api/index.mjs
3. Delete `rcc/wasm-dashboard/` (confirm with Rocky first)
4. Delete `rcc/heartbeat-local/` (confirm with Rocky first)
5. Confirm and delete `rcc/crush-server/` if disabled (confirm with Rocky — 630 files)

**Phase 2 — Config consolidation (prerequisite for everything else):**
6. Implement `rcc.json` config file + `rcc/lib/config.mjs` loader (wq-RCC-setup-001)
7. Move all mandatory + high-value env vars to rcc.json with env fallback
8. Document which `OMGJKH_BOT` / `AGENTFS_URL` / `WORKSPACE_DIR` vars are legacy

**Phase 3 — Route extraction (breaks the monolith):**
9. Extract `rcc/api/routes/queue.mjs` — the queue routes are the most self-contained
10. Extract `rcc/api/routes/agents.mjs` — heartbeat + registry
11. Extract `rcc/lib/storage.mjs` — all JSON I/O helpers
12. Extract remaining route groups one at a time, test after each

**Phase 4 — Delegation (shrinks RCC's surface area):**
13. Evaluate delegating Slack notifications to openclaw (removes ~200 lines + credentials)
14. Audit all LLM calls in api/index.mjs — ensure all go through tokenhub
15. Reconcile ClawBus: pick one implementation (in-process vs standalone), delete the other

**Phase 5 — HTML cleanup (cosmetic but meaningful):**
16. Confirm with Rocky which inline HTML functions are still served in production
17. Delete dead HTML functions (dashboardHtml at minimum — superseded by WASM dashboard)
18. Any surviving HTML pages → extract to `rcc/api/static/` as standalone files

---

*This audit is a read-only findings document. No code has been changed. All deletion candidates require Rocky's confirmation before action — production is live on do-host1.*

*Last updated: 2026-04-01 by Natasha*
