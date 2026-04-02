# RCC Rust Migration Spec
**Author:** Bullwinkle 🫎 (wq-JKH-003)  
**Date:** 2026-04-02  
**Directive:** jkh — "NO NODE.JS after this. Everything Rust/WASM."

---

## Current State

### Already in Rust ✅
| Component | Location | Status |
|-----------|----------|--------|
| Dashboard UI | `dashboard/dashboard-ui/` | Leptos/WASM, ships as SPA |
| Dashboard Server | `dashboard/dashboard-server/` | Axum proxy, serves WASM + proxies API/SC/Bus |
| ClawChat Server | `dashboard/squirrelchat-server/` | Axum + SQLite/WAL, FTS5, WebSocket |
| WASM Dashboard (v2) | `wasm-dashboard/` | Leptos, standalone views |

### Still in Node.js 🔴
| Component | Location | Lines | What it does |
|-----------|----------|-------|-------------|
| API Core | `api/index.mjs` | ~2132 | HTTP server, routing, auth, shared state init |
| API Routes (split) | `api/routes/*.mjs` | ~4985 | queue, agents, bus, agentos, memory, ui, services, projects, setup |
| Brain | `brain/index.mjs` | 299 | LLM dispatch via TokenHub |
| Vector | `vector/index.mjs` + `ingest.mjs` | 880 | Milvus embeddings + ingest |
| Lessons | `lessons/index.mjs` | 467 | Distributed lessons ledger |
| Issues | `issues/index.mjs` | 372 | GitHub issues cache |
| Scout | `scout/pump.mjs` + `github.mjs` | 644 | GitHub repo watcher, work item generator |
| LLM | `llm/client.mjs` + `registry.mjs` | 529 | PeerLLMClient, endpoint registry |
| Exec | `exec/agent-listener.mjs` | 332 | ClawBus exec listener |
| Crush Server | `crush-server/failover.mjs` | 321 | Failover/HA coordinator |
| Guardrails | `guardrails/adaptive-trust.mjs` | 296 | Trust scoring |
| Decision Journal | `decision-journal/` | ~241 | Intent drift detection |
| Trust | `trust/adaptive-trust.mjs` | 193 | Adaptive trust scoring |
| **Total Node.js** | | **~11,691** | |

### SOA Route Port Progress (from `303c3b9`, `3f52bab`)
Rocky has started porting routes natively into Rust (rcc-server binary). SOA cleanup (`d110c3c`) filed tasks `wq-SOA-001..017`. Some routes already native, most still proxied through dashboard-server → Node.js.

---

## Architecture Target

```
┌─────────────────────────────────────────────────┐
│                  rcc-server                       │
│            (single Rust binary)                   │
│                                                   │
│  ┌───────────┐ ┌───────────┐ ┌──────────────┐   │
│  │ API routes │ │ Bus/SSE   │ │ ClawChat │   │
│  │ (axum)    │ │ (axum)    │ │ (axum+sqlite)│   │
│  └───────────┘ └───────────┘ └──────────────┘   │
│  ┌───────────┐ ┌───────────┐ ┌──────────────┐   │
│  │ Brain     │ │ Scout     │ │ Vector/Milvus│   │
│  │ (reqwest) │ │ (octocrab)│ │ (reqwest)    │   │
│  └───────────┘ └───────────┘ └──────────────┘   │
│  ┌───────────┐ ┌───────────┐                     │
│  │ Lessons   │ │ Exec      │                     │
│  │ (sqlite)  │ │ (tokio)   │                     │
│  └───────────┘ └───────────┘                     │
├─────────────────────────────────────────────────┤
│  Static: WASM dashboard SPA (Leptos, served by   │
│  tower-http ServeDir)                             │
└─────────────────────────────────────────────────┘
```

**One binary.** `rcc-server` replaces both the Node.js API and dashboard-server. ClawChat stays as a sub-router within the same binary (already Axum, just move the crate).

---

## Data Model

### SQLite (migrate from JSON files)
| Table | Source | Notes |
|-------|--------|-------|
| `queue_items` | `data/queue.json` | Work queue with full history |
| `agents` | `data/agents.json` | Agent registry + capabilities |
| `heartbeats` | in-memory + `data/heartbeats/` | Recent heartbeats, prune >24h |
| `secrets` | `data/secrets.json` | Encrypted at rest with `ring` |
| `users` | `data/users.json` | Auth: username, email, token_hash, confirmed_at |
| `lessons` | `data/lessons.json` | Lessons ledger |
| `projects` | `data/projects.json` | Project registry |
| `conversations` | `data/conversations.json` | Agent conversation metadata |
| `requests` | `data/requests.json` | Work request history |
| `decision_journal` | `data/decision-journal.jsonl` | Append-only log |

### Keep as JSON (config files, not data)
- `rcc.json` — consolidated config (replaces 58 env vars)
- `capabilities-manifest.json` — agent capabilities
- `llm-registry.json` — LLM endpoint registry

### In-Memory Only
- Heartbeat map (agents → last heartbeat, with 24h TTL)
- SSE subscriber list (ClawBus stream)
- WebSocket connections (ClawChat)

---

## Dependency Stack

```toml
[dependencies]
# Web framework
axum = { version = "0.8", features = ["macros", "ws"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "fs", "compression-gzip"] }
tokio = { version = "1", features = ["full"] }

# Data
rusqlite = { version = "0.33", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# HTTP client (for TokenHub, GitHub, Milvus)
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }

# GitHub API
octocrab = "0.43"

# Auth
ring = "0.17"           # HMAC-SHA256 token hashing
base64 = "0.22"
uuid = { version = "1", features = ["v4"] }

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Email (for auth confirmation flow)
lettre = { version = "0.11", features = ["tokio1-native-tls"] }

# Misc
chrono = { version = "0.4", features = ["serde"] }
```

**No ORM.** Direct `rusqlite` with prepared statements. The data model is simple enough that an ORM adds more complexity than it removes.

---

## Module-by-Module Migration Plan

### Phase 1: Core API (highest leverage)
**Replaces:** `api/index.mjs` + `api/routes/queue.mjs` + `api/routes/agents.mjs`

These are the most-hit endpoints. Every agent calls them on every heartbeat cycle.

| Endpoint | Method | Rust module | Priority |
|----------|--------|-------------|----------|
| `/api/heartbeat/:agent` | POST | `routes/agents.rs` | P0 |
| `/api/heartbeats` | GET | `routes/agents.rs` | P0 |
| `/api/agents` | GET | `routes/agents.rs` | P0 |
| `/api/agents/:name` | GET/PATCH | `routes/agents.rs` | P0 |
| `/api/queue` | GET | `routes/queue.rs` | P0 |
| `/api/queue/item` | POST | `routes/queue.rs` | P0 |
| `/api/item/:id` | GET/PATCH/DELETE | `routes/queue.rs` | P0 |
| `/api/health` | GET | `routes/health.rs` | P0 |
| `/api/secrets/:key` | GET/POST | `routes/secrets.rs` | P0 |

**Estimated effort:** 2-3 days (most logic is simple CRUD)

### Phase 2: Communication Layer
**Replaces:** `api/routes/bus.mjs` + `exec/agent-listener.mjs`

| Endpoint | Method | Rust module | Priority |
|----------|--------|-------------|----------|
| `/bus/send` | POST | `routes/bus.rs` | P1 |
| `/bus/messages` | GET | `routes/bus.rs` | P1 |
| `/bus/stream` | GET (SSE) | `routes/bus.rs` | P1 |
| `/bus/heartbeat` | POST | `routes/bus.rs` | P1 |
| `/bus/presence` | GET | `routes/bus.rs` | P1 |

SSE streaming is straightforward in Axum with `axum::response::Sse`.

**Estimated effort:** 1-2 days

### Phase 3: Intelligence Layer
**Replaces:** `brain/index.mjs` + `vector/` + `scout/`

| Component | Rust module | Notes |
|-----------|-------------|-------|
| Brain (LLM dispatch) | `services/brain.rs` | reqwest → TokenHub `/v1/chat/completions` |
| Vector (Milvus) | `services/vector.rs` | reqwest → Milvus API + TokenHub embeddings |
| Scout (GitHub) | `services/scout.rs` | `octocrab` crate for GitHub API |
| LLM Registry | `services/llm_registry.rs` | In-memory registry with JSON persistence |

**Estimated effort:** 2-3 days (mostly HTTP client calls)

### Phase 4: Supporting Services
**Replaces:** `lessons/` + `issues/` + `crush-server/` + `guardrails/` + `trust/` + `decision-journal/`

| Component | Rust module | Notes |
|-----------|-------------|-------|
| Lessons | `services/lessons.rs` | SQLite table, simple CRUD |
| Issues (GH cache) | `services/issues.rs` | In-memory cache + periodic refresh |
| Failover | `services/failover.rs` | Health tracking + auto-failover logic |
| Trust scoring | `services/trust.rs` | Numerical scoring, no external deps |
| Decision journal | `services/journal.rs` | Append-only SQLite table |

**Estimated effort:** 1-2 days

### Phase 5: UI + Admin
**Replaces:** `api/routes/ui.mjs` + `api/routes/services.mjs` (Slack, setup wizard, etc.)

| Component | Rust module | Notes |
|-----------|-------------|-------|
| UI page serving | DELETE | WASM SPA handles all UI; Axum just serves index.html |
| Slack integration | `services/slack.rs` | Webhook receiver + `chat.postMessage` via reqwest |
| Setup wizard | `routes/setup.rs` | SSE progress stream + config writer |
| Package registry | DELETE | Low-value, appears 3x in current code, not worth porting |

**Estimated effort:** 1 day

### Phase 6: Auth System
**Already partially in Rust** (ClawChat has auth). Consolidate.

| Component | Rust module | Notes |
|-----------|-------------|-------|
| User registration | `auth/mod.rs` | Register, confirm email, login, reset |
| Token validation | `auth/middleware.rs` | Axum middleware, HMAC-SHA256 |
| Email confirmation | `auth/email.rs` | `lettre` for SMTP |

**Estimated effort:** 1-2 days (SC auth code can be extracted and shared)

---

## What Gets DELETED (not ported)

| Item | Reason |
|------|--------|
| `geek-view-reference.mjs` (2700 lines) | Reference for WASM port — already done |
| Package registry HTML (3 copies) | Low-value feature, not used by agents |
| `SPARKY_OLLAMA_URL` env var | Use LLM registry instead |
| `OMGJKH_BOT` env var | Move to secrets store |
| agentOS routes (`/api/agentos/*`) | Move to agentOS repo, not RCC's concern |
| All server-side HTML rendering | WASM SPA does all rendering |
| Mattermost integration code | Retired per `994f64e` |
| Duplicate route definitions | Audit P3 — dead code |

---

## Migration Strategy

**Incremental, not big-bang.** The dashboard-server already proxies all `/api/*` requests to the Node.js API. As each route is ported to Rust:

1. Add the native Axum handler
2. Register it BEFORE the catch-all proxy route (Axum matches first route)
3. Node.js route becomes dead code
4. After all routes in a module are ported, delete the Node.js file

This means the system is always working — never a partial migration with broken endpoints.

### Milestone Checkpoints

| Milestone | Routes ported | Node.js remaining | Status |
|-----------|---------------|-------------------|--------|
| M0 (now) | Proxy-only + some SOA routes | ~100% | ✅ |
| M1 | Core API (heartbeats, agents, queue, secrets) | ~60% | 🎯 Next |
| M2 | + Bus/SSE + Auth | ~35% | |
| M3 | + Brain + Vector + Scout | ~15% | |
| M4 | + Lessons + Issues + Trust + Journal | ~5% | |
| M5 | + Slack + Setup | 0% — delete Node.js | |

### Per-Milestone Deliverable
Each milestone ships with:
- Native Axum routes passing integration tests
- Node.js proxy removed for ported routes
- Updated systemd unit (`rcc-server.service`) using Rust binary
- `make test` covering new Rust routes

---

## Config Consolidation

Replace 58 env vars with `rcc.json`:

```json
{
  "server": {
    "port": 8789,
    "external_url": "https://rcc.jordanhubbard.net"
  },
  "auth": {
    "agent_token": "...",
    "admin_token": "..."
  },
  "data": {
    "dir": "./data",
    "queue_path": "./data/queue.db",
    "secrets_path": "./data/secrets.db"
  },
  "tokenhub": {
    "url": "http://localhost:8090",
    "admin_token": "..."
  },
  "milvus": {
    "url": "http://localhost:19530"
  },
  "slack": {
    "bot_token": "...",
    "signing_secret": "...",
    "agent_channel": "..."
  },
  "squirrelbus": {
    "url": "https://dashboard.jordanhubbard.net",
    "token": "..."
  },
  "github": {
    "token": "..."
  },
  "email": {
    "smtp_host": "...",
    "smtp_port": 587,
    "from": "rcc@jordanhubbard.net"
  }
}
```

Load order: `rcc.json` → env vars (override) → CLI flags (override). Env vars still work for docker/CI, but `rcc.json` is the canonical config.

---

## Build + Deploy

```bash
# Build single binary
cargo build --release -p rcc-server

# Run
./target/release/rcc-server --config ./rcc.json

# Or via systemd
sudo cp deploy/rcc-server.service /etc/systemd/system/
sudo systemctl enable --now rcc-server
```

**Cross-compilation targets:**
- `x86_64-unknown-linux-gnu` (Rocky/DO, Sweden fleet)
- `aarch64-unknown-linux-gnu` (Natasha/Sparky)
- `aarch64-apple-darwin` (Bullwinkle/puck)

Use `cross` for Linux targets from macOS, or build on each host.

---

## Task Assignment Recommendation

| Phase | Best assignee | Reason |
|-------|--------------|--------|
| Phase 1 (Core API) | Rocky | Owns the codebase, already started SOA port |
| Phase 2 (Bus/SSE) | Rocky | Bus is tightly coupled to API server |
| Phase 3 (Brain/Vector/Scout) | Natasha | GPU/vector experience, TokenHub expertise |
| Phase 4 (Supporting) | Bullwinkle | Lessons/trust are clean, good for parallel work |
| Phase 5 (UI/Admin) | Any — mostly deletion | |
| Phase 6 (Auth) | Bullwinkle or Snidely | SC auth already exists, extraction task |

---

## Success Criteria

- [ ] `rcc-server` binary boots and serves all endpoints natively (no Node.js process)
- [ ] `rcc.json` replaces all env vars
- [ ] SQLite replaces all JSON data files (with migration script)
- [ ] All existing integrations (heartbeats, queue, bus, SC, dashboard) work unchanged
- [ ] `node` binary is not required on the server
- [ ] Single `curl | sh` install works for new hosts
- [ ] Binary size < 50MB (reasonable for a self-contained server)
- [ ] Startup time < 2 seconds

---

_Nothing up my sleeve... PRESTO!_ 🫎
