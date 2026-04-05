# Shared Infrastructure Design — Agent Coherence Layer
_Rocky · 2026-04-02_

> Problem: "I cannot do task X because agent Y wrote the file I want in her own local filesystem."
> 
> Goal: Every agent operates on the same logical memory, filesystem, and task graph.
> No agent should need to ask another agent "can you share that file?" or "what do you remember about X?"

---

## 1. What We Have Today

| Resource | Current State | Problem |
|----------|--------------|---------|
| Memory (files) | Per-agent local markdown: `~/.openclaw/workspace/memory/YYYY-MM-DD.md` | Natasha's memories are invisible to Rocky. Boris can't read anything. |
| Vector DB | Milvus on do-host1, used only by Rocky + partial Natasha | Sweden containers write nothing. Most agents don't embed anything. |
| Filesystem | Local per-agent disk | Classic "file lives on Natasha's machine" problem. |
| Object store | MinIO on do-host1, has `agents/{name}/` buckets | Bucket structure exists but nobody reads from it systematically. |
| Task system | CCC queue.json on do-host1 | Already shared ✅ — this is the one thing that works. |
| Bus | ClawBus (SSE/JSONL on do-host1) | Already shared ✅ |

The task system and bus are already coherent. The problem is memory, knowledge, and files.

---

## 2. Target Architecture

```
Every agent reads/writes:
  ┌─────────────────────────────────────────────────────────────┐
  │  ClawFS (S3/MinIO-backed virtual filesystem)               │
  │    s3://agents/shared/     ← fleet-wide shared files        │
  │    s3://agents/{name}/     ← per-agent private (still S3)  │
  │    s3://agents/workspace/  ← ephemeral working files        │
  ├─────────────────────────────────────────────────────────────┤
  │  VectorDB (Milvus on do-host1, all agents write/read)       │
  │    rcc_memory      ← all agent memories (scoped by agent)  │
  │    rcc_messages    ← cross-agent conversations              │
  │    rcc_lessons     ← fleet-wide lessons learned            │
  │    rcc_queue       ← work item semantic dedup               │
  ├─────────────────────────────────────────────────────────────┤
  │  Task Queue (CCC queue.json → Dolt → /api/queue)           │
  │    Already shared ✅                                        │
  ├─────────────────────────────────────────────────────────────┤
  │  ClawBus (SSE pub/sub on do-host1)                      │
  │    Already shared ✅                                        │
  └─────────────────────────────────────────────────────────────┘
```

---

## 3. Shared Filesystem (ClawFS on MinIO)

### 3a. Bucket layout

```
agents/                        ← single bucket on do-host1 MinIO
  shared/                      ← read/write by all agents
    MEMORY.md                  ← fleet-wide long-term memory
    heartbeats/{agent}.json    ← live heartbeat data
    squirrelbus.jsonl          ← bus log snapshot
    workspace/{agent}/         ← agent's shared working files
  {agent}/                     ← private per-agent (read by owner, read by Rocky)
    memory/YYYY-MM-DD.md       ← daily notes
    state.json                 ← agent state
    config.json                ← agent config
  wasm/                        ← compiled WASM modules (agentOS)
    {hash}.wasm
  artifacts/                   ← build outputs, rendered files
    {project}/{filename}
```

### 3b. Access model

| Path | Who can read | Who can write | Notes |
|------|-------------|---------------|-------|
| `shared/MEMORY.md` | All agents | All agents (append preferred, merge on conflict) | Fleet-wide memory |
| `shared/workspace/{agent}/` | All agents | Owner agent | Agent's shared working files |
| `{agent}/memory/` | Owner + Rocky | Owner | Daily notes stay semi-private |
| `shared/heartbeats/` | All agents | Owner | Heartbeat data, 60s TTL |
| `wasm/` | All agents | Rocky + owner | WASM module store |
| `artifacts/` | All agents | Any agent | Build outputs |

### 3c. OpenClaw integration

Add an `clawfs` skill / config to OpenClaw that wraps S3 read/write:
- `clawfs.read(path)` → streams file from MinIO
- `clawfs.write(path, content)` → writes to MinIO
- `clawfs.list(prefix)` → lists objects under prefix
- `clawfs.exists(path)` → HEAD check

For file writes that conflict (two agents writing `shared/MEMORY.md` simultaneously):
- Prefer **append-only writes** with agent-stamped blocks
- CCC reconciles on a schedule (last-write-wins by ts for non-append fields)
- Files that are truly collaborative (MEMORY.md) use a append-log format, periodically compacted by Rocky

### 3d. Transparent mount for OpenClaw workspace

Each agent's OpenClaw workspace should mount:
- `~/.openclaw/workspace/memory/` → backed by `s3://agents/{agent}/memory/`
- `~/.openclaw/workspace/shared/` → backed by `s3://agents/shared/workspace/{agent}/`

Implementation: `s3fs` or `rclone mount` on startup. Or (simpler): OpenClaw's `read`/`write` tools route through ClawFS when `AGENTFS_ENABLED=true`.

The simplest viable approach: **periodic sync** rather than FUSE mount.
- Agent writes to local `~/.openclaw/workspace/memory/` (as today)
- Sync daemon (`clawfs-sync`) mirrors to/from S3 every 60s
- Rocky's reconciler merges conflicts (timestamp wins, manual review for MEMORY.md)

---

## 4. Shared Vector Memory (Milvus)

### 4a. Current state

Milvus collections exist: `rcc_memory`, `rcc_lessons`, `rcc_queue`, `rcc_messages`, `rcc_memory_sparky`.

The problem: only Rocky and Natasha (partial) write to them. Sweden containers write nothing. Agents don't systematically embed their memories.

### 4b. Target: every memory write = vector embed

When any agent writes a memory entry (daily note, lesson, conversation), it should:
1. Write to local file (fast, local)
2. Call CCC `/api/memory/ingest` (async, non-blocking) with the text + metadata
3. CCC embeds via tokenhub → upserts to Milvus `rcc_memory`

When any agent needs context:
1. Call CCC `/api/memory/recall?q=...&agent=...` 
2. Returns semantically similar memories from **all agents** (scoped by relevance, not by agent)
3. Agent uses this context alongside its local MEMORY.md

### 4c. Milvus schema evolution

`rcc_memory` should have:
```
id          VarChar(128)  PK    — hash(agent + ts + content[:64])
vector      FloatVector(3072)   — text-embedding-3-large via tokenhub
agent       VarChar(32)         — which agent wrote this
content     VarChar(4096)       — the memory text
source      VarChar(64)         — 'daily-note' | 'lesson' | 'conversation' | 'task' | 'file'
ts          Int64               — unix timestamp ms
project     VarChar(128)        — optional project/repo context
scope       VarChar(32)         — 'private' | 'shared' | 'fleet'
```

Filter on `scope`:
- `private` → only returned when queried by the owning agent
- `shared` → returned to any agent querying with relevant context
- `fleet` → always returned to all agents (lessons, architecture decisions)

### 4d. Agent memory write path

Current (broken):
```
Agent → writes ~/.openclaw/workspace/memory/2026-04-02.md (local only)
```

Target:
```
Agent writes MEMORY.md
  ↓
clawfs-sync mirrors to s3://agents/{agent}/memory/2026-04-02.md
  ↓
CCC ingest trigger (webhook on S3 event OR periodic scan)
  ↓
CCC embeds content via tokenhub (text-embedding-3-large)
  ↓
Upserts to Milvus rcc_memory (scope=private for daily notes, scope=fleet for lessons)
  ↓
Available to all agents via /api/memory/recall
```

### 4e. Cross-agent recall

When Rocky needs to know "what was Natasha working on last week?":
```
GET /api/memory/recall?q=what+was+Natasha+working+on&agent=natasha&k=10
```
Returns top-10 semantically similar memories from `rcc_memory` where `agent=natasha`.

When any agent needs to recall context for a task (agent=any):
```
GET /api/memory/recall?q=how+to+restart+vllm+safely&k=5
```
Returns memories from all agents, ranked by relevance. Rocky's `vllm-watchdog` lesson appears, Natasha's GPU recovery notes appear.

---

## 5. Eventual Coherence Model

### Why "eventually coherent" is fine here

We don't need strong consistency. We need:
- No agent is **permanently blind** to another agent's knowledge
- Reads return **recent enough** data (minutes, not days)
- Writes don't block the writing agent

The natural convergence window:
- Filesystem (S3 sync): 60s
- Vector memory (ingest on write): <30s via async CCC call
- Task queue: already real-time via CCC

This is fine for AI agent workloads. An agent can work with knowledge that's 60 seconds stale.

### Conflict resolution

| Resource | Strategy |
|----------|----------|
| Per-agent files (daily notes) | Append-only; last write wins for same agent |
| `shared/MEMORY.md` | Rocky owns compaction; agents append with `## [{agent} {ts}]` blocks |
| Milvus vectors | Upsert by stable ID; no conflict (content-addressed ID via hash) |
| Queue items | Already conflict-free (optimistic lock via `version` field) |

---

## 6. Implementation Plan

### Phase 1 — ClawFS (S3 sync, 2 weeks)

**wq-AGENTFS-001**: CCC `/api/fs/*` endpoint suite
- `GET /api/fs/read?path=shared/MEMORY.md`
- `POST /api/fs/write` (path, content, agent, scope)
- `GET /api/fs/list?prefix=shared/`
- `DELETE /api/fs/delete?path=...`
- Backed by MinIO S3 (do-host1, bucket `agents`)
- Auth: bearer token (same as rest of CCC)

**wq-AGENTFS-002**: `clawfs-sync` daemon for each agent
- Lightweight: watches `~/.openclaw/workspace/memory/` + `~/.openclaw/workspace/shared/`
- On write: uploads to S3 immediately (debounced 5s)
- On startup: downloads agent's S3 files to local workspace
- Runs as systemd unit or openclaw plugin
- Config: `AGENTFS_BUCKET`, `MINIO_URL`, `MINIO_ACCESS_KEY`, `MINIO_SECRET_KEY` (from CCC secrets)

**wq-AGENTFS-003**: deploy clawfs-sync to all agents via bootstrap
- Add to `deploy/bootstrap.sh`: pull `clawfs-sync` binary, start as systemd unit
- Add MINIO creds to CCC secrets → distributed via `secrets-sync.sh`

### Phase 2 — Fleet Memory (vector ingest, 1 week)

**wq-VECMEM-001**: CCC `/api/memory/ingest` → Milvus pipeline (Rust, SOA-007)
- Receives text + metadata from any agent
- Embeds via tokenhub `/v1/embeddings`
- Upserts to `rcc_memory` (content-addressed ID, scope field)
- Rate-limited to prevent embed quota burn

**wq-VECMEM-002**: automatic memory ingest triggers
- OpenClaw hook: on `memory/` file write → POST to `/api/memory/ingest`
- This makes every agent's daily note searchable fleet-wide within 30s

**wq-VECMEM-003**: cross-agent recall in OpenClaw context
- When OpenClaw builds context for a new session: query `/api/memory/recall` for recent fleet memories relevant to the session topic
- Results injected into system context (capped at 2K tokens to avoid bloat)

### Phase 3 — Coherence tooling (1 week)

**wq-COHERENCE-001**: memory compaction cron
- Rocky runs nightly: reads all agent daily notes from S3, extracts significant entries, upserts to fleet `rcc_memory` with `scope=fleet`, prunes stale local copies
- Keeps fleet memory lean without losing individual agent history

**wq-COHERENCE-002**: fleet workspace view in dashboard
- Dashboard shows `ClawFS` tab: browse shared S3 files, see recent writes per agent
- Search via vector recall from UI

**wq-COHERENCE-003**: agent self-registration with ClawFS
- On OpenClaw startup: register agent in `agents/shared/registry.json`
- Heartbeat writes to `agents/shared/heartbeats/{agent}.json`
- Rocky's geek view reads from S3 directly (no more "agent offline but files are there" confusion)

---

## 7. What This Solves

| Problem | Solution |
|---------|----------|
| "Natasha wrote a file I need" | All non-private files live in S3, available to all agents |
| "Rocky doesn't know what Boris is working on" | Boris writes heartbeat + task updates to `shared/heartbeats/boris.json` |
| "I forgot what we discussed last week" | Vector recall from `rcc_memory` surfaces it from any agent's notes |
| "Sweden containers can't reach Natasha's files" | S3 over HTTP via CCC proxy — no direct agent-to-agent TCP needed |
| "Two agents wrote the same lesson" | Milvus dedup by content-addressed ID (hash of text); near-duplicates surface as the same embedding |
| "I need all agents to know about this architecture decision" | Write to `shared/MEMORY.md` with `scope=fleet` → embedded and available to all |

---

## 8. Non-Goals / Intentional Constraints

- **No FUSE mount** — too fragile for agents with flaky network (Sweden containers). S3 HTTP API only.
- **No strong consistency** — 60s eventual coherence is fine. Don't pay distributed transaction overhead.
- **No per-agent Milvus instances** — one central Milvus, all agents are just clients. Simpler, fewer failure modes.
- **No vector embedding at agent runtime** — always delegate to CCC → tokenhub. Agents don't need local GPU for memory ops.
- **No filesystem ACL complexity** — `private` is enforced by convention and CCC auth, not by Milvus/MinIO permissions. Agents are trusted.
- **ClawFS is not a general-purpose distributed filesystem** — it's a document store for agent knowledge and working files. Large binary files go to `artifacts/`, not `memory/`.

---

## 9. Open Questions for jkh

1. **MinIO replication**: currently single-node on do-host1. Should we add a replica on sparky? Or is do-host1 availability acceptable (it's already the CCC host)?
2. **Sweden containers → MinIO**: they can reach do-host1 via the reverse SSH tunnel. MinIO at `http://127.0.0.1:9000` via tunnel, or public at `http://146.190.134.110:9000`? Need a decision before deploying clawfs-sync to Boris/Peabody etc.
3. **Shared MEMORY.md vs per-agent**: should the fleet `shared/MEMORY.md` replace or supplement per-agent `MEMORY.md`? My recommendation: supplement — per-agent MEMORY.md stays for personal context, fleet MEMORY.md is for architecture/project knowledge.
4. **clawfs-sync language**: Rust (fits the migration) or Go (simpler one-binary deploy like tokenhub)? I'd go Rust for consistency with the migration.

---

_Filed by: Rocky (do-host1) · 2026-04-02_
