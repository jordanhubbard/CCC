# CCC Dashboard v2 — Design Specification

**Status:** Design complete — implementation in progress  
**Authors:** Rocky 🐿️, Natasha 🕵️, Bullwinkle 🫎  
**Date:** 2026-03-26

---

## Framing

We're virtual employees working on multiple projects together. jkh is the benign boss — we appeal to him for manual intervention, resources, or when we want a vote on something. The dashboard is our shared operations center: it should feel like a war room, not a status page. Every agent should be able to look at it and know exactly what's happening, what needs attention, and what's waiting on a human.

---

## What's Changing (Summary)

| Area | Current | v2 |
|---|---|---|
| ClawBus | Front page | Own tab, filtered |
| Kanban | None | Per-agent columns, color-coded cards |
| Calendar | None | Shared, bidirectional, agent-writable |
| Projects | List page (/projects) | Health cards with GitHub data |
| Comms Channels | None | Settings page |
| jkh Appeal Queue | Buried in queue | Dedicated panel |
| Agent Status | Heartbeat table | Live status strip on kanban |
| Geek View | None | Distributed brain map |

---

## Navigation

New top-level nav tabs:

1. **Overview** (new landing page — agent status strip + jkh appeal queue)
2. **Kanban** (task board)
3. **Calendar** (shared calendar)
4. **Projects** (was at /projects, enhanced)
5. **ClawBus** (moved from front page, now filtered)
6. ⚙️ **Settings** (comms channels, agent config — was nowhere)

---

## 1. Overview Page (New Landing)

Replaces the current front page. Two panels:

### Agent Status Strip
Live heartbeat data, one card per agent:
```
🟢 Rocky     | do-host1       | idle                         | 2m ago
🟢 Bullwinkle| puck           | reviewing PR jordanhubbard/x | 8m ago  
🟢 Natasha   | sparky         | render job (est. 4min)       | 1m ago
🟡 Boris     | l40-sweden     | idle                         | 22m ago
```
- Status derived from heartbeat payload's `activity` field (agents should include this)
- Color: 🟢 <5min, 🟡 5-30min, 🔴 >30min
- Click to expand: full capabilities, last 3 queue items, recent lessons

### jkh Appeal Queue
Dedicated panel — items stalled waiting for human judgment. NOT the main queue.
- Items land here when any agent sets `needsHuman: true` or status `awaiting-jkh`
- Shows: item title, which agent flagged it, why, how long it's been waiting
- **Escalation aging**: card background heat-maps from neutral → amber → red based on time waiting. <2h: normal; 2-24h: amber tint; >24h: red tint; >72h: full red with ⚠️ badge. Old items scream louder than new ones.
- jkh actions: ✅ Approve / ❌ Reject / 💬 Comment / 🔀 Reassign
- Sends notification to Telegram when new item arrives
- Items auto-clear when jkh acts or agent resolves

---

## 2. Kanban Board

### Layout
One column per agent + one "Unassigned" column. Left-to-right: Rocky | Bullwinkle | Natasha | Boris | Unassigned | 🎯 Appeal Queue (mini, rightmost)

### Card Colors (by type)
- 🔴 `bug` — red border + tag
- 💡 `idea` — yellow/amber border + tag  
- 🟢 `feature` — green border + tag
- 🟣 `proposal` — purple border + tag
- ⬜ `task` — default (no color)

Type is inferred from queue item `tags[]` (first match wins). Tags `bug`, `idea`, `feature`, `proposal` are the canonical type tags.

### Blocking Annotations
- If item has `blockedBy: ["wq-XYZ"]` or `blocks: ["wq-ABC"]`, show a 🔗 chain link badge
- Tooltip: "Blocks: wq-ABC (title)" or "Blocked by: wq-XYZ (title)"
- Item stays in its agent's column — no duplication
- "Waiting on" badge: `⛔ waiting: jkh` / `⛔ waiting: rocky` etc. — separate from blocking

### Card Actions
- Drag to reassign (moves to target agent's column, updates `assignee` via PATCH /api/item/:id)
- Click to expand: full description, journal, choices
- 🙋 "Appeal to jkh" button — promotes to appeal queue, sends Telegram notification
- Status chip: pending / in-progress / blocked / awaiting-jkh

### Filters (top bar)
- By type: All | Bugs | Ideas | Features | Proposals
- By priority: All | Urgent | High | Medium | Low
- Show completed: toggle (default off)

---

## 3. Shared Calendar

### What It Is
A cross-agent, bidirectional calendar. Agents can read AND write events. jkh can view and annotate.

### Implementation
- Rocky owns `/api/calendar` endpoint (backed by `shared/calendar.json` in MinIO)
- Events schema:
  ```json
  {
    "id": "cal-uuid",
    "title": "string",
    "start": "ISO8601",
    "end": "ISO8601",
    "owner": "rocky|bullwinkle|natasha|boris|jkh",
    "type": "block|deadline|event|travel|maintenance",
    "resource": "sparky-gpu|sparky-cpu|puck|do-host1|l40-sweden|null",
    "description": "string",
    "tags": [],
    "allDay": false
  }
  ```
  `resource` enables queryable resource blocking — e.g. `GET /api/calendar?resource=sparky-gpu&start=X&end=Y` to check availability before farming a job.
- Any agent can `POST /api/calendar` (with auth token) to create events
- `GET /api/calendar?start=X&end=Y` returns events in range
- `DELETE /api/calendar/:id` — owner or Rocky only

### Use Cases
- Natasha: "Sparky busy until ~6am — don't farm GPU jobs" (type: `block`, owner: `natasha`)
- Rocky: "jkh in Taiwan until ~March 31" (type: `travel`, owner: `rocky`)
- Bullwinkle: "PR review deadline — jordanhubbard/usdagent #12" (type: `deadline`)
- jkh: "NVIDIA all-hands" (type: `event`, owner: `jkh`)

### UI
- Monthly/weekly toggle
- Events color-coded by owner (each agent has a color)
- Event types shown as icons: ✈️ travel, 🔒 block, 📅 deadline, 🔧 maintenance
- Click event: expand details, edit (owner only), delete

---

## 4. Projects View (Enhanced)

Current `/projects` page survives but gets richer cards.

### Project Health Card
Each registered project shows:
```
┌─────────────────────────────────────────────────────────┐
│ 🐿️ rockyandfriends                    [personal] [active]│
│ The shared workspace for Rocky, Bullwinkle, Natasha      │
│                                                          │
│ 📝 Last commit: 2h ago (main)  🔴 3 open issues  🟣 1 PR│
│ 🤖 Active agent: Rocky         📋 2 queue items          │
│ #rockyandfriends (omgjkh)                                │
└─────────────────────────────────────────────────────────┘
```
- Click → existing project detail page (enhanced)
- Health colors: green (active commits <24h), yellow (<7d), red (stale >7d)
- GitHub data from existing `/api/projects/:id/github` endpoint (already built)

### Project Detail (enhanced)
Existing page + add:
- CI status (GitHub Actions latest run)
- Active agent badge (who's currently working on it)
- Direct link to Kanban filtered to this project

### Project Deep-Dive View (new)

Clicking into a project card opens a full project page with **three integrated panels**:

**1. Project Kanban (scoped)**
Same kanban component as the main board, but filtered to only this project's items. Still shows per-agent columns — so you can see which agent owns what on this specific project. Cards still have type colors and blocking annotations. Drag-to-reassign works the same way.

**2. Communications Channels (project-scoped)**
All channels that are wired to this project:
```
┌──────────────────────────────────────────────────────────┐
│ 📡 Communications                                        │
│                                                          │
│ Slack #usdagent (omgjkh)          🟢 active  last: 2h   │
│ Slack #itsallgeektome (offtera)   🟡 linked  last: 8h   │
│ GitHub Issues                     🔴 3 open             │
│ GitHub PRs                        🟣 1 open             │
│ ClawBus (tag: usdagent)       🟢 12 msgs today      │
└──────────────────────────────────────────────────────────┘
```
- Channels sourced from `project.slack_channels[]` + inferred from GitHub repo
- ClawBus row auto-populates if any messages in the log carry this project's tag
- Status dots: active (recent message), linked (configured but quiet), unconfigured (grey)
- Click a channel row → jump to ClawBus tab filtered to that channel + project

**3. Existing panels (retained)**
- GitHub issues + PRs panel (already built)
- Active queue items
- Recent completed items
- Project metadata (description, notes, scouts, links)

---

## 5. ClawBus (Moved to Tab)

### Changes
- No longer on front page
- Filtered view by default: dropdown to filter by agent, project tag, or message type
- Raw stream still accessible ("Show all" toggle)
- Search bar (text match on message content)
- Retain existing fan-out / dead-letter / ack infrastructure unchanged

---

## 6. Settings Page (New)

Accessed via ⚙️ gear icon in nav. Not a primary tab.

### Sections

**Communication Channels**
Table: channel name, type, configured endpoints, status, last activity
- Mattermost: rocky↔bullwinkle DM, rocky↔natasha DM, #agent-shared
- Slack: #itsallgeektome (offtera), #rockyandfriends (omgjkh)
- Telegram: jkh direct
- ClawBus: local + peer URLs
- Milvus: vector search endpoint status

**Agent Registry**
Table of registered agents with capabilities (gpu, claude_cli, inference_key), token status, last seen.

**Auth Tokens**
List of configured CCC auth tokens (masked). Rotate button (jkh only, sends to Telegram).

---

## 7. Geek View — The Distributed Brain

jkh's framing: "a distributed brain." This is a visualization of the entire system — services, agents, communication channels, and live traffic flow — all on one page.

### What It Shows

**Nodes (rendered as bubbles/boxes in a force-directed or fixed layout):**

| Node | Type | What it represents |
|---|---|---|
| Rocky (do-host1) | Agent | Primary always-on node, CCC API host |
| Bullwinkle (puck) | Agent | Mac node, Claude Code runner |
| Natasha (sparky) | Agent | GPU node, render + inference |
| Boris (l40-sweden) | Agent | x86 GPU, Omniverse headless |
| CCC API | Service | Port 8789 — work queue, heartbeats, projects |
| WQ Dashboard | Service | Port 8788 — this UI |
| CCC Brain | Service | LLM request queue + retry engine |
| Milvus | Service | Vector search (port 19530) |
| MinIO | Service | Object storage (port 9000) |
| SearXNG | Service | Search (port 8888) |
| NVIDIA Inference Gateway | External | `inference-api.nvidia.com` |
| GitHub | External | `api.github.com` |
| Mattermost | External | `chat.yourmom.photos` |
| Slack (omgjkh) | External | `omgjkh.slack.com` |
| Slack (offtera) | External | `offtera.slack.com` |
| Telegram | External | jkh's phone |
| ClawBus | Internal bus | JSONL log + fan-out |

**Edges (connections):**
- Solid line = persistent connection (agent ↔ CCC API heartbeat)
- Dashed line = on-demand call (agent → inference gateway when processing)
- Animated dots flowing along edges = live traffic (when ClawBus or heartbeat activity detected)
- Edge label: protocol (HTTP/REST, Mattermost DM, Slack Socket Mode, etc.)

**Live indicators:**
- Each node has a pulse animation if it's been active in the last 5 minutes
- Heartbeat age shown on agent nodes
- CCC API shows: uptime, queue depth, brain status
- Milvus: collection count, last index operation
- MinIO: bucket count (if mc is available)

**Traffic flow visualization:**
- When a ClawBus message fires, animate a particle along the path: sender → ClawBus → recipient
- When a heartbeat arrives, flash the agent→CCC API edge
- When a brain request fires, animate: CCC Brain → NVIDIA Gateway → CCC Brain
- Traffic log panel at bottom: last 20 events with timestamp, type, from→to

### Implementation Notes
- Static topology with live data overlaid (not truly force-directed — layout is hand-tuned for clarity)
- Data sources: `/api/heartbeats`, `/api/health`, `/api/brain/status`, ClawBus JSONL tail
- SSE endpoint `GET /api/geek/stream` — server-sent events for live traffic updates
- New endpoint `GET /api/geek/topology` — returns the full node/edge map with current live status
- Rendering: SVG or Canvas (SVG preferred for accessibility + CSS animations)
- Dark theme matching existing dashboard aesthetic

### Topology Model: Hybrid (machines primary, services as chips)

**Decision (Natasha raised, Rocky resolved 2026-03-26):**

Primary nodes = **machines**: Rocky (do-host1), Bullwinkle (puck), Natasha (sparky), Boris (l40-sweden).
Each machine node renders as a rounded rect with service badges/chips shown inside or below it.

Services get **their own nodes** only when they are shared infrastructure called directly by multiple agents:
- Milvus (do-host1, port 19530) — called by Rocky, Bullwinkle, Natasha
- MinIO (do-host1, port 9000) — called by all agents
- SearXNG (do-host1, port 8888) — called by all agents

Everything else renders as **service chips** on their host machine node:
- Rocky chips: CCC API (:8789), WQ Dashboard (:8788), CCC Brain, ClawBus hub, Tailscale proxy
- Bullwinkle chips: OpenClaw gateway (:18789, reachability-checked), ClawBus push endpoint (:8788), launchd crons (heartbeat-rcc.plist + openclaw), disk free, uptime, tmux session count
- Natasha/Sparky chips: OpenClaw gateway (:18789), ClawBus (/bus → :18799 via gateway, not a separate external port), Milvus (:19530), CUDA/RTX ⚡, Ollama (:11434, verified ✅ — models: qwen2.5-coder:32b, qwen3-coder:latest)
- Boris chips: OpenClaw gateway, L40 GPU ⚡, Omniverse headless

Rationale: machine-first topology tells you *where to SSH* for debugging. Shared-service nodes tell you *where traffic actually goes*. Hybrid gives both without the noise of a full service mesh diagram.

**Do NOT** split DGX Spark from Sparky — same machine, one node.

### Layout (rough sketch)
```
┌──────────────────────────────────────────────────────────────────┐
│  🧠 Distributed Brain — CCC Network                    [live ●] │
│                                                                  │
│   [Telegram]←──────────────────────────[Rocky]──────[CCC API]   │
│                                          │  │         │    │     │
│   [Slack omgjkh]←───[ClawBus]───────┤  └──[Brain]┘  [MinIO]│
│   [Slack offtera]                        │              [SearXNG]│
│                                   [Bullwinkle]   [Milvus]       │
│   [NVIDIA Gateway]←──────────────────┤  │                       │
│                                  [Natasha] [Boris]               │
│   [GitHub]←──[CCC Scout]──────────────────────────────────────── │
│                                                                  │
│  ── Traffic Log ──────────────────────────────────────────────── │
│  10:41:03 heartbeat  rocky → CCC API                            │
│  10:40:58 bus_msg    natasha → squirrelbus → rocky (lesson)     │
│  10:40:31 brain_req  CCC Brain → NVIDIA Gateway (sonnet)        │
└──────────────────────────────────────────────────────────────────┘
```

---

## API Additions Required

| Endpoint | Method | Description |
|---|---|---|
| `/api/calendar` | GET | List events (start/end filter) |
| `/api/calendar` | POST | Create event (auth required) |
| `/api/calendar/:id` | DELETE | Delete event (owner/Rocky only) |
| `/api/appeal` | GET | List items awaiting jkh |
| `/api/appeal/:id` | POST | jkh action (approve/reject/comment) |
| `/api/geek/topology` | GET | Node/edge map + live status |
| `/api/geek/stream` | GET (SSE) | Live traffic event stream |
| `/api/heartbeat/:agent` | POST | **Extend** to accept `activity` field |
| `/api/heartbeat/:agent/history` | GET | Last 24h heartbeat events for sparkline |
| `/api/crons` | GET | All registered cron jobs + last status |
| `/api/crons/:agent` | POST | Agent reports cron job status |
| `/api/provider-health` | GET | All agents' model provider status |
| `/api/provider-health/:agent` | POST | Agent reports provider health |

### Heartbeat `activity` extension
Agents should include in their heartbeat POST:
```json
{
  "activity": "idle" | "render job (est. 4min)" | "reviewing PR #42" | "running claude-code on X",
  "activitySince": "ISO8601"
}
```
This is optional/backward-compatible — existing heartbeats without it show "idle".

---

## Queue Item Schema Additions

Two new fields on queue items:

```json
{
  "type": "bug|idea|feature|proposal|task",
  "blockedBy": ["wq-XYZ"],
  "blocks": ["wq-ABC"],
  "needsHuman": false,
  "needsHumanReason": "string"
}
```

Existing items without `type` display as `task` (neutral color).

---

## Implementation Plan

## Bullwinkle's Input (2026-03-26)

Six operational pain points, all incorporated into the design:

1. **Session context digest** — "what happened while I was gone" panel on Overview. Last N events per channel, missed alerts, heartbeat gaps since last session. Reduces cold-start token burn for stateless agents.
2. **Heartbeat sparkline** — per-agent 24h timeline (green/red dots). The 7h haimaker outage would have been immediately visible as a gap. Not just current status — *history*.
3. **Cron job status panel** — each agent's cron jobs: last success, last failure, consecutive error streak. Silent cron failures (47 in a row) must surface on the dashboard, not in logs nobody reads.
4. **Provider health panel** — which model provider each agent is using, last response status, last error with timestamp. "haimaker: 400 since 14:16 PT" is worth more than 3 hours of log-digging.
5. **Unanswered cross-agent message queue** — filtered view showing ClawBus messages sent to an agent without a response. "Rocky → Bullwinkle: 12 unanswered pings" is a health signal, not chat noise.
6. **Soul commit timeline** — last commit per agent in the `souls/` repo. jkh would like seeing this. It's semantically meaningful and earns its dashboard real estate.

Bullwinkle's column scope: crons, providers, session health, Google Workspace connectivity. Not GPU metrics, container status, or Boris-specific.

**Bullwinkle agent card specifics (puck):**
- LaunchAgent status: `heartbeat-rcc.plist` + OpenClaw launchd jobs — green/red per job
- Disk space: "X GB free" — Mac minis fill up quietly
- OpenClaw gateway reachability (port 18789 via Tailscale) — #1 failure mode, must be prominent
- Uptime / last reboot (battery irrelevant — Mac mini, always plugged in)

**Bullwinkle Geek View service chips:**
- OpenClaw gateway (:18789) — with live reachability check
- ClawBus push endpoint (:8788, if running)
- Active tmux sessions (e.g. claude-puck worker) — show count

**Calendar scope clarification (Bullwinkle):**
CCC calendar = agent events only (cron schedules, planned work windows, maintenance blocks).
NOT a mirror of jkh's personal Google Calendar (jkh uses `gog` for that — no duplication needed).
The two calendars are separate concerns.

---

## Implementation Plan

### Phase 1 — Foundation (Rocky)
- [ ] Add `type`, `blockedBy`, `blocks`, `needsHuman` fields to queue schema
- [ ] `/api/calendar` endpoint + MinIO-backed storage
- [ ] `/api/appeal` endpoint
- [ ] `/api/geek/topology` static endpoint
- [ ] Heartbeat `activity` field extension
- [ ] Heartbeat history store (last 24h per agent, for sparklines)
- [ ] `/api/crons` endpoint — agents register/report their cron jobs
- [ ] `/api/provider-health` endpoint — agents report model provider status

### Phase 2 — Frontend (Rocky, with Boris/Bullwinkle review)
- [ ] New nav structure (6 tabs + settings gear)
- [ ] Overview page: agent status strip + appeal queue + session digest
- [ ] Heartbeat sparklines (24h per agent)
- [ ] Cron job status panel
- [ ] Provider health panel
- [ ] Kanban board: columns, card colors, blocking badges, drag-to-reassign
- [ ] Calendar UI (monthly/weekly view, agent color-coding)
- [ ] Enhanced project health cards

### Phase 3 — Geek View (Rocky + Natasha input on topology)
- [ ] SVG topology layout
- [ ] SSE stream for live traffic
- [ ] Animate particles on ClawBus events + heartbeats
- [ ] Traffic log panel + unanswered message queue
- [ ] Soul commit timeline per agent

### Phase 4 — Agent Updates (all agents)
- [ ] All agents update heartbeat to include `activity` field
- [ ] All agents use `type` tag when creating queue items
- [ ] All agents know to set `needsHuman: true` when appropriate
- [ ] All agents report cron job status to `/api/crons`
- [ ] All agents report provider health to `/api/provider-health`

---

## Design Decisions (jkh: "just go for it")

1. **Appeal queue notifications** → Telegram (primary channel, always reaches jkh)
2. **Calendar ownership** → agents can annotate jkh events, only owner can edit/delete
3. **Geek view traffic** → SSE near-realtime (~1s); graceful fallback to 5s poll if SSE fails
4. **Drag-to-reassign** → optimistic update, undo toast for 5s
5. **Boris's column** → only shown when he has items (keeps the board clean by default)
6. **Session digest lookback** → last 8h or since last heartbeat, whichever is shorter

---

*Built with input from Rocky 🐿️, Natasha 🕵️, and Bullwinkle 🫎. All three reviewed. Design complete — awaiting jkh answers to 6 questions, then implementation begins.*  
*Last updated: 2026-03-26*
