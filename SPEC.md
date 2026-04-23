# Spec: Auto-Dispatch, Idle Discovery, and Idea Voting

## Objective

Fleet tasks sit unclaimed indefinitely; idle agents have nothing to do; good improvement ideas never surface or get validated. This feature adds three interlocking capabilities to `acc-server`:

1. **Auto-dispatch** — a server-side Tokio task that routes unclaimed fleet tasks to agents proactively, with backfill for stale tasks
2. **Idle discovery** — agents with no work are automatically assigned "discovery" tasks that explore projects and propose ideas
3. **Idea voting** — proposed ideas require 3-of-4 agent agreement (with mandatory refinements) before being promoted to real work tasks

**Target users:** Fleet operators (tasks stop piling up), agents (always have purposeful work), and the project backlog (continuously enriched with validated ideas).

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│  dispatch.rs — Tokio task, ticks every 15s          │
│                                                     │
│  tick()                                             │
│    ├─ backfill_stale_tasks()      (startup + tick)  │
│    ├─ dispatch_unclaimed_tasks()                    │
│    │    ├─ Phase 1: directed nudge → chosen agent   │
│    │    ├─ Phase 2: broadcast nudge → all capable   │
│    │    └─ Phase 3: explicit server-side claim      │
│    ├─ detect_idle_agents()                          │
│    │    └─ create_discovery_task() per idle agent   │
│    └─ tally_idea_votes()                            │
│         └─ promote_to_work_task() when 3 approve    │
└─────────────────────────────────────────────────────┘
```

Everything runs inside `acc-server` as a single `tokio::spawn`'d async fn. No new binary, no new service, no new Cargo dependencies.

---

## Part 1: Auto-Dispatch

### Three-Phase Dispatch

For each open, unblocked task, the dispatch loop tracks time-since-created (using `metadata.dispatch.last_nudge_at`):

```
Task created
    │
    ├─ immediately → directed nudge to best-matched online agent
    │                 message: tasks:dispatch_nudge  to: <agent-name>
    │                 agent wakes poll loop, races to claim
    │
    ├─ after 30s unclaimed → broadcast nudge to ALL capable online agents
    │                         message: tasks:dispatch_nudge  to: null (broadcast)
    │
    └─ after 90s still unclaimed → server explicitly claims for best agent
                                    SQL: UPDATE ... WHERE status='open'
                                    message: tasks:dispatch_assigned  to: <agent-name>
                                    agent discovers it on next poll
```

### Backfill Mode

On first tick (and on each subsequent tick, until cleared), any task with `created_at` older than `ACC_DISPATCH_BACKFILL_THRESHOLD` (default: 3600s / 1 hour) that is still open is treated as if it has already passed all timeouts and goes directly to Phase 3 (explicit assignment).

This handles the queue of tasks (like the NanoLang bugs) that have been sitting for hours.

### Capability Matching

When selecting an agent for directed nudge or explicit assignment:
1. Fetch agents from `state.agents` where `lastSeen` within 300s (online)
2. Filter: agent `capabilities` must include `metadata.preferred_executor` (or any agent if none required)
3. Filter: agent must not be in `metadata.dispatch.blacklist` (agents that were assigned and failed)
4. Score: fewest current `claimed` tasks in `fleet_tasks` (least-loaded wins)
5. Tiebreak: agent name alphabetically (deterministic)

### Dispatch State in Metadata

Stored in existing `metadata` JSON column — no schema migration:
```json
{
  "dispatch": {
    "nudge_count": 2,
    "last_nudge_at": "2026-04-23T10:00:00Z",
    "assign_attempts": 1,
    "last_assign_at": "2026-04-23T10:01:30Z",
    "blacklist": ["agent-that-failed"]
  }
}
```

### Agentbus Message Types

**Directed nudge** (Phase 1):
```json
{
  "type": "tasks:dispatch_nudge",
  "to": "hermes-prod-01",
  "task_id": "task-uuid",
  "project_id": "NanoLang",
  "task_type": "work",
  "priority": 1
}
```

**Broadcast nudge** (Phase 2): same, `"to": null`

**Explicit assignment** (Phase 3):
```json
{
  "type": "tasks:dispatch_assigned",
  "to": "hermes-prod-01",
  "task_id": "task-uuid",
  "project_id": "NanoLang"
}
```

### Agent-Side Changes (`agent/acc-agent/src/tasks.rs`)

- Subscribe to agentbus SSE stream (already exists)
- On `tasks:dispatch_nudge` where `to` matches own name OR `to` is null: immediately trigger poll cycle if below capacity
- On `tasks:dispatch_assigned` where `to` matches own name: immediately poll to discover and begin the assigned task

---

## Part 2: Idle Discovery

### Idle Detection

An agent is considered idle when:
- It has 0 tasks with `status IN ('claimed', 'in_progress')` in `fleet_tasks`
- It has been online (heartbeat within 300s) for at least `ACC_IDLE_GRACE_PERIOD` seconds (default: 120s — avoids false positives on startup)

### Discovery Task Creation

Discovery tasks are **always priority 4** (lowest). They are never dispatched by the normal task dispatch path — only the idle-detection path creates and assigns them.

An agent is eligible for a discovery task only when:
- It is online (heartbeat within 300s)
- It has been online for at least `ACC_IDLE_GRACE_PERIOD` (120s)
- It has **zero** tasks in `fleet_tasks` with `status IN ('claimed', 'in_progress')`
- There are **no open non-discovery tasks** available that this agent could claim (checked against `GET /api/tasks?status=open` filtered by agent capability) — discovery only fills a truly empty queue

When all conditions are met, the dispatch loop checks: does any project have zero open idea tasks?

If yes, create a discovery task and directly assign it to the idle agent:

```json
{
  "project_id": "<project-with-no-ideas>",
  "title": "Explore [project] and propose improvement ideas",
  "description": "You have no assigned work. Explore this project's codebase, recent tasks, and open issues. Identify gaps, inefficiencies, or missing capabilities. For each meaningful finding, create an idea task using POST /api/tasks with task_type=idea.",
  "task_type": "discovery",
  "priority": 4,
  "claimed_by": "<idle-agent>",
  "claimed_at": "<now>",
  "status": "claimed",
  "metadata": { "auto_created": true, "trigger": "idle" }
}
```

Selection priority for which project gets a discovery task: projects with the **oldest last-activity timestamp** (last task created_at or completed_at) first.

If all projects already have open idea tasks, no discovery task is created — agent stays idle, and that's fine.

Only one discovery task per idle agent at a time. If the agent already holds an uncompleted discovery task, no new one is created.

---

## Part 3: Idea Tasks and Voting

### New Task Types

Two new values for `task_type`:
- `discovery` — auto-created by dispatch loop, assigned to idle agents; agent explores and proposes ideas
- `idea` — proposed improvement; requires voting before becoming real work

### Idea Task Lifecycle

```
discovery task → agent creates idea tasks via POST /api/tasks (task_type=idea)
                                        │
                         status: open (awaiting votes)
                         dispatch loop nudges other agents to vote
                                        │
                    ┌───────────────────┼───────────────────┐
                 vote 1              vote 2              vote 3
              (+ refinement)      (+ refinement)      (+ refinement)
                                        │
                           3 approvals reached?
                                  │
                   ┌──────────────┴──────────────┐
                  YES                             NO (rejected or expired)
                   │                              │
          promote to work task          rejected → status: rejected
          (new task, type=work)         expiring → Rocky asks user first
          merging all refinements                  (see Pre-Expiry Escalation)
```

### Idea Voting Nudges

When an idea task is created (or on each tick while it has fewer than 3 votes and is not yet rejected), the dispatch loop publishes a `tasks:dispatch_nudge` to agents that:
- Are online
- Have not yet voted on this idea
- Are not the creator of the idea

Nudge cadence: once per `ACC_DISPATCH_TICK` until the idea has 3 votes, is rejected, or expires. Agents receiving the nudge check their capacity; if below max tasks they pick up the vote as a lightweight action (vote tasks do not count toward `ACC_MAX_TASKS_PER_AGENT` capacity — they are advisory).

### Pre-Expiry Escalation (Rocky)

When an idea task reaches `created_at + (ACC_IDEA_VOTE_EXPIRY - ACC_IDEA_EXPIRY_WARN_BEFORE)` (default: warn 24h before 7-day expiry, i.e. at day 6) and has not yet reached the promotion threshold:

1. The dispatch loop sets `metadata.expiry_warned=true` on the idea task
2. Publishes a `rocky:ask_human` message on the agentbus addressed `to: "rocky"`:
```json
{
  "type": "rocky:ask_human",
  "to": "rocky",
  "subject": "Idea expiring soon",
  "body": "The idea '[title]' (project: [project_id]) expires in ~24h with [N] votes. Should I extend it, promote it anyway, or let it expire?\n\nCurrent votes:\n[vote summary]",
  "idea_task_id": "task-uuid",
  "actions": ["extend_7d", "promote_anyway", "let_expire"]
}
```
3. Rocky relays this to the user (via Telegram/Slack/web per their active channel) and waits for a reply
4. Rocky responds to the bus with `rocky:human_response` carrying the chosen action
5. Dispatch loop handles: extend (update `metadata.expiry_extended_at`), promote (force-promote ignoring vote count), or let expire (mark `status=rejected`, `metadata.expired=true`)

If Rocky is offline or does not respond within `ACC_ROCKY_RESPONSE_TIMEOUT` (default: 4h), the idea expires normally.

### Voting API

**Cast a vote:**
```
PUT /api/tasks/:id/vote
{
  "agent": "hermes-worker-02",
  "vote": "approve" | "reject",
  "refinement": "string — required, describes what you'd change or add"
}
```
- `refinement` is **required** — agents must contribute something before their vote counts
- An agent can only vote once per idea (subsequent calls update their existing vote)
- Returns 409 if the task is not `task_type=idea`
- Returns 409 if the agent created the idea (can't self-vote)

**Vote state** stored in `metadata.votes`:
```json
{
  "votes": [
    {
      "agent": "hermes-worker-02",
      "vote": "approve",
      "refinement": "Add error handling for the edge case where...",
      "voted_at": "2026-04-23T11:00:00Z"
    }
  ]
}
```

### Promotion Logic

On each dispatch tick, `tally_idea_votes()` scans all open idea tasks:
- Count unique `approve` votes where `refinement` is non-empty
- If approvals >= 3: **promote** — create a new `work` task merging:
  - Original title + description from the idea task
  - All refinements appended to the description as "Agent refinements: ..."
  - `priority` inherited from idea task
  - `project_id` inherited
  - `metadata.promoted_from`: the idea task ID
  - Mark the idea task `status=completed`, `metadata.promoted=true`
- If rejections > 1 (more than 1 agent rejects): mark `status=rejected` — idea is archived

The "best 3 of 4" framing means: with up to 4 voters, 3 approvals is a supermajority; 2 rejections kills the idea. This prevents a single dissenter from blocking while requiring genuine consensus.

### Discovery Task Completion

When an agent completes a discovery task, it should have created ≥1 idea tasks. The dispatch loop does not enforce this (agents are trusted), but the completion is recorded normally.

---

## Data Model Changes

No new tables. All new state fits in existing columns:

| Field | Where | Purpose |
|---|---|---|
| `task_type` | existing column | new values: `discovery`, `idea` |
| `metadata.dispatch` | existing JSON | nudge/assign tracking, blacklist |
| `metadata.votes` | existing JSON | idea votes + refinements |
| `metadata.auto_created` | existing JSON | marks server-generated tasks |
| `metadata.promoted_from` | existing JSON | traces work task back to idea |

---

## Configuration

| Variable | Default | Description |
|---|---|---|
| `ACC_DISPATCH_ENABLED` | `true` | Kill switch for entire dispatch loop |
| `ACC_DISPATCH_TICK` | `15` | Seconds between ticks |
| `ACC_DISPATCH_NUDGE_AFTER` | `30` | Seconds before broadcast nudge |
| `ACC_DISPATCH_ASSIGN_AFTER` | `90` | Seconds before explicit assignment |
| `ACC_DISPATCH_MAX_ASSIGN_ATTEMPTS` | `3` | Give up after N explicit assignment failures |
| `ACC_DISPATCH_BACKFILL_THRESHOLD` | `3600` | Seconds old before backfill treatment |
| `ACC_IDLE_GRACE_PERIOD` | `120` | Seconds online before agent considered idle |
| `ACC_IDEA_APPROVE_THRESHOLD` | `3` | Approvals needed to promote idea |
| `ACC_IDEA_REJECT_THRESHOLD` | `2` | Rejections needed to archive idea |
| `ACC_IDEA_VOTE_EXPIRY` | `604800` | Seconds (7 days) before unvoted idea auto-archives |
| `ACC_IDEA_EXPIRY_WARN_BEFORE` | `86400` | Seconds before expiry to warn Rocky (default: 24h) |
| `ACC_ROCKY_RESPONSE_TIMEOUT` | `14400` | Seconds to wait for Rocky's human response before expiring anyway (default: 4h) |

---

## Project Structure

```
acc-server/src/
  dispatch.rs          ← NEW: Tokio task — all dispatch, idle, idea logic
  main.rs              ← MODIFIED: tokio::spawn(dispatch::run(state.clone()))
  routes/tasks.rs      ← MODIFIED: add /api/tasks/:id/vote endpoint;
                                    trigger directed nudge on task create
  state.rs             ← no change (bus_tx + agents + fleet_db all present)

agent/acc-agent/src/
  tasks.rs             ← MODIFIED: react to dispatch_nudge / dispatch_assigned bus msgs
```

---

## Code Style

- Existing Rust/Axum/Tokio/rusqlite patterns throughout
- No new Cargo dependencies
- Dispatch logic is pure functions — unit testable without I/O
- Log all dispatch decisions: `tracing::info!(task_id, agent, phase, "dispatching")`
- Voting endpoint follows same auth pattern as existing task endpoints

---

## Testing Strategy

### Unit Tests (dispatch.rs)
- `test_capability_match_no_requirement` — all agents match when no executor required
- `test_capability_match_gpu_only` — only GPU agents match GPU tasks
- `test_select_least_loaded_agent` — ties broken alphabetically
- `test_backfill_threshold` — tasks > 1h old skip to phase 3
- `test_idle_agent_detection` — agent with 0 tasks + 120s grace = idle
- `test_discovery_not_created_when_real_work_available` — available non-discovery tasks block discovery assignment
- `test_discovery_not_duplicated` — agent already holding discovery task gets no second one
- `test_idea_tally_promotes_at_3` — 3 approvals → promotion with merged refinements
- `test_idea_tally_rejects_at_2` — 2 rejections → archived
- `test_idea_self_vote_blocked` — creator cannot vote on own idea
- `test_idea_voting_nudge_excludes_voted_agents` — agents who already voted not re-nudged
- `test_expiry_warn_fires_at_day6` — `rocky:ask_human` published at `expiry - warn_before`
- `test_expiry_warn_not_duplicated` — `expiry_warned=true` prevents second warning
- `test_rocky_timeout_expires_idea` — no Rocky response within 4h → idea expires normally

### Integration Tests
- `test_new_task_directed_nudge` — create task → directed nudge on bus within one tick
- `test_broadcast_after_timeout` — mock 30s elapsed → broadcast nudge fires
- `test_explicit_assign_after_90s` — mock 90s elapsed → task claimed by server
- `test_backfill_on_startup` — pre-existing old tasks assigned on first tick
- `test_idle_creates_discovery_task` — agent with no tasks and no available work → discovery task created
- `test_idea_vote_requires_refinement` — vote without refinement → 400
- `test_idea_promotion_creates_work_task` — 3 approvals → new work task with merged refinements
- `test_rocky_escalation_extend` — Rocky responds "extend_7d" → expiry pushed forward

### Manual Acceptance Criteria
- [ ] New task in dashboard → claimed within 30s with agent online
- [ ] Task sitting for >1h → claimed within one tick on deploy
- [ ] Agent finishes all tasks with no other work available → discovery task auto-assigned within one tick
- [ ] Agent with available real work → no discovery task created
- [ ] Agent creates idea task → other agents receive nudge to vote (not the creator)
- [ ] 3 agents vote approve + add refinements → work task appears in dashboard
- [ ] Idea at day 6 → Rocky asks user before expiry; user can extend, promote, or let expire
- [ ] Rocky offline for 4h → idea expires without intervention
- [ ] `ACC_DISPATCH_ENABLED=false` → nothing dispatched, no discovery tasks, no promotions

---

## Boundaries

**Always do:**
- Use atomic SQL claim (`WHERE status='open'`) — never bypass
- Require non-empty `refinement` before counting a vote
- Respect `blocked_by` — never dispatch a task with incomplete blockers
- Respect `claim_expires_at` — don't dispatch already-claimed, unexpired tasks

**Ask first about:**
- Dispatching tasks across projects to agents not registered for that project
- Reducing `ACC_DISPATCH_ASSIGN_AFTER` below 60s
- Changing vote thresholds below 3/2

**Never do:**
- Allow an agent to vote on its own idea
- Dispatch to agents with `lastSeen` > 300s
- Touch the legacy `/api/queue` system
- Add new database tables (use `metadata` JSON)
- Promote an idea without at least 3 votes that include non-empty refinements
- Assign a discovery task to an agent that has available real work or an existing discovery task
- Expire an idea without first attempting Rocky escalation (unless Rocky is confirmed offline/unresponsive)
