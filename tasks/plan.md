# Implementation Plan: Auto-Dispatch, Idle Discovery, and Idea Voting

Source spec: `SPEC.md`
Target files: `acc-server/src/dispatch.rs` (new), `acc-server/src/main.rs`, `acc-server/src/routes/tasks.rs`, `agent/acc-agent/src/tasks.rs`

---

## Dependency Graph

```
T1 (dispatch skeleton)
T2 (capability matcher)
    │
    ├──► T3 (directed nudge on create)
    │         └──► T4 (agent reacts to nudge)   ◄─── checkpoint 1
    │
    └──► T5 (tick loop: broadcast + assign + backfill)
              └──► T11 (idle detection)
                        └──► T12 (discovery task creation)  ◄─── checkpoint 3

T6 (vote API)  [independent]
    ├──► T7 (vote nudges in dispatch loop)
    └──► T8 (idea tally + promotion)
              ├──► T9 (Rocky pre-expiry warning)
              └──► T10 (Rocky response handler)   ◄─── checkpoint 2
```

**Parallelisable pairs:** (T1, T2), (T3, T6), (T4, T5), (T7, T8), (T9, T10), (T11, T12)

---

## Tasks

### T1 — Dispatch skeleton: module + tick loop + main.rs spawn

**What:** Create `acc-server/src/dispatch.rs` with a `pub async fn run(state: Arc<AppState>)` that loops on a configurable interval. Wire it into `main.rs` via `tokio::spawn`. Read all `ACC_DISPATCH_*` env vars with defaults.

**Files changed:**
- `acc-server/src/dispatch.rs` (new)
- `acc-server/src/main.rs` — add `tokio::spawn(dispatch::run(state.clone()))`
- `acc-server/src/lib.rs` — expose `pub mod dispatch`

**Acceptance criteria:**
- Server starts without panic; log line `[dispatch] tick loop started` appears
- `ACC_DISPATCH_ENABLED=false` → loop exits immediately with log `[dispatch] disabled`
- `ACC_DISPATCH_TICK=5` → tick fires every 5s (verified via log timestamps)
- No new Cargo dependencies added

---

### T2 — Capability matcher: pure functions

**What:** In `dispatch.rs`, implement `select_best_agent(task: &Value, agents: &Value, claimed_counts: &HashMap<String, usize>) -> Option<String>` as a pure function (no I/O). Filters online agents by capability match; scores by load; breaks ties alphabetically.

**Acceptance criteria (unit tests only, no running server needed):**
- `test_capability_match_no_requirement` — task with no `preferred_executor` → all agents eligible
- `test_capability_match_specific_executor` — task requiring `"gpu"` → only agents with `capabilities.gpu` truthy
- `test_select_least_loaded_agent` — agent-a has 2 tasks, agent-b has 1 → picks agent-b
- `test_tiebreak_alphabetical` — equal load → picks lexicographically first name
- `test_no_eligible_agents` — returns `None` gracefully
- `test_blacklisted_agent_excluded` — agent in `metadata.dispatch.blacklist` not selected

---

### T3 — Directed nudge on task create

**What:** In `routes/tasks.rs` `create_task`, after the existing `tasks:added` bus publish, call a new `dispatch::nudge_task(state, &task)` helper. This helper calls `select_best_agent`, then publishes `tasks:dispatch_nudge` with `"to": "<agent-name>"` on `bus_tx`. If no capable agent online, publishes with `"to": null` (broadcast).

**Files changed:**
- `acc-server/src/routes/tasks.rs` — call nudge after insert
- `acc-server/src/dispatch.rs` — add `pub fn nudge_task(...)` (sync, takes `&broadcast::Sender<String>`)

**Acceptance criteria:**
- Create a task via `POST /api/tasks` → `tasks:dispatch_nudge` appears on bus within the same request
- If capable agent is online: message has `"to": "<agent-name>"`
- If no capable agent online: message has `"to": null`
- Existing `tasks:added` message still fires (no regression)

---

### T4 — Agent reacts to bus nudge messages

**What:** In `agent/acc-agent/src/tasks.rs`, spawn a background task that subscribes to the agentbus SSE stream (`GET {acc_url}/bus/stream`). On receiving `tasks:dispatch_nudge` where `to` matches own agent name OR `to` is null, signal the main poll loop to run immediately rather than waiting for the next `POLL_IDLE` tick.

Use a `tokio::sync::Notify` shared between the bus subscriber and the main loop.

**Files changed:**
- `agent/acc-agent/src/tasks.rs` — add bus subscription, `Notify`-based wake

**Acceptance criteria:**
- Agent polling loop wakes within 2s of receiving a directed nudge (verified in logs)
- Agent at capacity (`active >= max_concurrent`) ignores the nudge (does not attempt claim)
- Agent correctly handles SSE stream disconnect — reconnects after 5s backoff
- `tasks:dispatch_assigned` with matching `to` also wakes the loop

---

### CHECKPOINT 1 — Basic dispatch end-to-end

Verify manually:
- [ ] Start server + one agent
- [ ] Create a task via API
- [ ] Agent log shows wake within 2s and claims task
- [ ] `ACC_DISPATCH_ENABLED=false` — agent does not wake on create, claims on next poll cycle only

---

### T5 — Tick loop: broadcast nudge + explicit assign + backfill

**What:** Implement the main dispatch tick body in `dispatch.rs`:

1. **Backfill** (runs every tick, handled first): query open tasks where `created_at < now - ACC_DISPATCH_BACKFILL_THRESHOLD` AND `metadata->>'$.dispatch.assign_attempts' < ACC_DISPATCH_MAX_ASSIGN_ATTEMPTS` (or field absent). Skip phase 1/2, go straight to explicit assignment.

2. **Nudge phase** (30s): for each open task where `metadata->>'$.dispatch.last_nudge_at'` is older than `ACC_DISPATCH_NUDGE_AFTER` seconds (or null): broadcast `tasks:dispatch_nudge` with `"to": null`; update `metadata.dispatch.nudge_count` and `last_nudge_at`.

3. **Assign phase** (90s): for each open task where `metadata->>'$.dispatch.last_nudge_at'` is older than `ACC_DISPATCH_ASSIGN_AFTER` seconds: call `select_best_agent`, execute atomic SQL claim, publish `tasks:dispatch_assigned` to `"to": <agent>`. Increment `assign_attempts`. If `assign_attempts >= max`, log and stop trying.

Lock `fleet_db` only for the duration of each SQL call; release between tasks to avoid blocking request handlers.

**Files changed:**
- `acc-server/src/dispatch.rs` — implement full tick body

**Acceptance criteria:**
- `test_backfill_skips_phases` — task with `created_at = now - 2h` gets explicit assign on first tick without nudge phase
- `test_nudge_fires_at_30s` — task age 35s → broadcast nudge published
- `test_assign_fires_at_90s` — task age 95s → SQL claim executed for best agent
- `test_max_assign_attempts_respected` — after N failures, task no longer dispatched (log warning)
- `test_blocked_task_skipped` — task with incomplete `blocked_by` not dispatched
- On deploy with existing stale tasks: all get assigned within first tick (manual)

---

### T6 — Vote API: `PUT /api/tasks/:id/vote`

**What:** Add vote endpoint to `routes/tasks.rs`. Validates: task exists, `task_type=idea`, agent != creator (read from `metadata.created_by` set at idea creation), `refinement` non-empty. Reads `metadata.votes` array, upserts this agent's vote entry, writes back. Returns updated task.

Also modify `create_task` to set `metadata.created_by = agent` when `task_type=idea` (agent must pass `"agent"` in the request body).

**Files changed:**
- `acc-server/src/routes/tasks.rs` — add `vote_on_task` handler, register route `PUT /api/tasks/:id/vote`; patch `create_task`

**Acceptance criteria:**
- `PUT /api/tasks/:id/vote` with valid body → 200, vote stored in `metadata.votes`
- Missing `refinement` field → 400 `{"error":"refinement required"}`
- Empty `refinement` string → 400
- Agent same as `metadata.created_by` → 409 `{"error":"cannot vote on own idea"}`
- `task_type != idea` → 409 `{"error":"task is not an idea"}`
- Voting twice updates existing vote entry (not duplicated)
- New idea task has `metadata.created_by` set to the requesting agent

---

### T7 — Vote nudges in dispatch loop

**What:** In the dispatch tick, scan `open` idea tasks with `< ACC_IDEA_APPROVE_THRESHOLD` approvals and `< ACC_IDEA_REJECT_THRESHOLD` rejections. For each, collect the set of online agents who have NOT yet voted and are not the creator. Publish a `tasks:dispatch_nudge` with `"to": <agent>` per eligible agent. Rate-limit: only send per-agent per-idea once per `ACC_DISPATCH_TICK` interval (no re-nudge within same tick cycle).

**Files changed:**
- `acc-server/src/dispatch.rs` — add `nudge_idea_voters()` called from tick

**Acceptance criteria:**
- New idea task → non-creator online agents each receive directed nudge within one tick
- Agent who has already voted → not nudged again
- Creator of idea → never nudged on their own idea
- Idea with 3 approvals → no more vote nudges sent
- Idea with 2 rejections → no more vote nudges sent

---

### T8 — Idea tally + promotion + rejection

**What:** In the dispatch tick, call `tally_idea_votes()`:
- Count votes where `vote=approve` AND `refinement` non-empty
- Count votes where `vote=reject`
- If approvals >= `ACC_IDEA_APPROVE_THRESHOLD`: create a new `work` task; description = original + `"\n\n---\n**Agent refinements:**\n" + each refinement bulleted`; set `metadata.promoted_from = idea_task_id`; mark idea `status=completed`, `metadata.promoted=true`; publish `tasks:added` for the new work task
- If rejections >= `ACC_IDEA_REJECT_THRESHOLD`: mark idea `status=rejected`, `metadata.expired=false`

**Files changed:**
- `acc-server/src/dispatch.rs` — add `tally_idea_votes()`

**Acceptance criteria:**
- `test_promotes_at_3_approvals` — 3 approve votes → new `work` task created with merged refinements; idea `status=completed`
- `test_rejects_at_2_rejections` — 2 reject votes → idea `status=rejected`
- `test_no_action_below_threshold` — 2 approvals, 1 rejection → no state change
- `test_promotion_inherits_project_and_priority` — promoted task has same `project_id` and `priority`
- `test_promotion_only_once` — already-completed idea not re-promoted on next tick

---

### CHECKPOINT 2 — Idea lifecycle end-to-end

Verify manually:
- [ ] Create an idea task via POST
- [ ] Two non-creator agents receive vote nudges within one tick
- [ ] Three agents vote approve with refinements → work task appears in dashboard
- [ ] Two agents vote reject → idea disappears from open tasks
- [ ] Creator cannot vote on own idea (API returns 409)

---

### T9 — Rocky pre-expiry warning

**What:** In the dispatch tick, call `check_idea_expiry()`:
- Query open idea tasks where `created_at < now - (ACC_IDEA_VOTE_EXPIRY - ACC_IDEA_EXPIRY_WARN_BEFORE)` AND `metadata->>'$.expiry_warned' IS NOT true`
- For each: set `metadata.expiry_warned = true`, build vote summary string, publish `rocky:ask_human` on `bus_tx` with fields from spec
- Also set `metadata.rocky_warn_sent_at = <now>` for timeout tracking

**Files changed:**
- `acc-server/src/dispatch.rs` — add `check_idea_expiry()`

**Acceptance criteria:**
- Idea at day 6 → `rocky:ask_human` published on bus exactly once
- `metadata.expiry_warned = true` prevents second warning on next tick
- Message body includes idea title, project, vote count, and the three action choices
- Idea not yet at day 6 → no warning published

---

### T10 — Rocky response handler

**What:** Add a new bus message handler in the dispatch tick (subscribe to a `tokio::sync::broadcast::Receiver` clone) that listens for `rocky:human_response` messages and acts:
- `extend_7d` → update `metadata.expiry_extended_at = <now>`; effective expiry = `created_at + VOTE_EXPIRY + 7d`
- `promote_anyway` → call the same promotion logic from T8, bypassing vote count check
- `let_expire` → mark task `status=rejected`, `metadata.expired=true`

Also: on each tick, expire any idea task where effective expiry < now AND `metadata.expiry_warned=true` AND `metadata.rocky_warn_sent_at < now - ACC_ROCKY_RESPONSE_TIMEOUT`.

**Files changed:**
- `acc-server/src/dispatch.rs` — add response listener + expiry enforcement

**Acceptance criteria:**
- `rocky:human_response` with `action=extend_7d` → expiry advances 7 days
- `action=promote_anyway` → work task created even with < 3 votes
- `action=let_expire` → idea immediately rejected
- Rocky offline for > 4h → idea expires via timeout path on next tick
- `test_rocky_timeout_expires_idea` unit test passes

---

### T11 — Idle agent detection

**What:** In the dispatch tick, call `detect_idle_agents()` which:
1. Reads online agents from `state.agents` (lastSeen within 300s)
2. For each, checks `fleet_tasks` for `claimed_by = <agent>` AND `status IN ('claimed','in_progress')` — count > 0 → not idle
3. Checks agent `metadata.online_since` (set on heartbeat) — if < `ACC_IDLE_GRACE_PERIOD` seconds ago → not yet idle
4. Checks open non-discovery, non-idea tasks that the agent is capable of claiming — any available → not idle (agent will claim via normal dispatch)
5. Returns list of truly-idle agent names

**Files changed:**
- `acc-server/src/dispatch.rs` — add `detect_idle_agents()`

**Acceptance criteria:**
- `test_idle_agent_with_no_tasks` — agent online 3min, 0 claimed tasks, no available work → returned as idle
- `test_not_idle_has_active_task` — agent with 1 claimed task → not idle
- `test_not_idle_grace_period` — agent online for 60s (< 120s grace) → not idle
- `test_not_idle_work_available` — open work task exists that agent can claim → not idle

---

### T12 — Discovery task auto-creation

**What:** For each idle agent from T11, call `maybe_create_discovery_task()`:
1. Check if agent already holds an open discovery task → skip if yes
2. Find projects with zero open `task_type=idea` tasks; sort by oldest last-activity (max `updated_at` across tasks for that project, ascending)
3. If no such project → skip (all projects have ideas; agent stays idle)
4. Create a `discovery` task directly assigned to the idle agent (`status=claimed`, `claimed_by=<agent>`, `priority=4`)
5. Publish `tasks:dispatch_assigned` to that agent on bus

**Files changed:**
- `acc-server/src/dispatch.rs` — add `maybe_create_discovery_task()`

**Acceptance criteria:**
- Idle agent + project with no ideas → discovery task created and assigned within one tick
- Idle agent + all projects have ideas → no task created
- Agent already has a discovery task → no second one created
- Discovery task has `priority=4` and `metadata.auto_created=true`
- `test_discovery_not_created_when_real_work_available` — if T11 filtered correctly, this is already guaranteed; verify via integration

---

### CHECKPOINT 3 — Full system end-to-end

Verify manually:
- [ ] Deploy with queue of stale tasks → all claimed within first tick
- [ ] All agents finish work → discovery tasks auto-assigned to each
- [ ] Agents create idea tasks from discovery → vote nudges flow to others
- [ ] Idea reaches 3 approvals → work task in dashboard
- [ ] Idea at day 6 → Rocky asks user in active channel
- [ ] `ACC_DISPATCH_ENABLED=false` → system fully quiet

---

## Summary

| Task | Depends on | Parallel with | Effort |
|------|-----------|---------------|--------|
| T1 dispatch skeleton | — | T2, T6 | S |
| T2 capability matcher | — | T1, T6 | S |
| T3 directed nudge on create | T1, T2 | T6 | S |
| T4 agent bus reaction | T3 | T5 | M |
| T5 tick loop (broadcast/assign/backfill) | T1, T2 | T4 | M |
| **Checkpoint 1** | T3, T4 | | |
| T6 vote API | — | T1, T2, T3 | S |
| T7 vote nudges in loop | T1, T6 | T8 | S |
| T8 idea tally + promotion | T6 | T7 | M |
| T9 Rocky expiry warning | T8 | T10 | S |
| T10 Rocky response handler | T8 | T9 | S |
| **Checkpoint 2** | T7, T8, T9, T10 | | |
| T11 idle detection | T5 | T12 | S |
| T12 discovery task creation | T11 | — | S |
| **Checkpoint 3** | T11, T12 | | |
