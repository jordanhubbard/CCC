# Task List: Auto-Dispatch, Idle Discovery, Idea Voting

See tasks/plan.md for full acceptance criteria and dependency graph.

## Phase 1 — Dispatch Foundation
- [ ] T1: dispatch.rs skeleton + tokio::spawn in main.rs
- [ ] T2: capability matcher pure functions + unit tests
- [ ] T3: directed nudge on task create (routes/tasks.rs)
- [ ] T4: agent wakes on bus nudge (agent/acc-agent/src/tasks.rs)
- [ ] T5: tick loop — broadcast nudge, explicit assign, backfill

**Checkpoint 1:** basic dispatch end-to-end

## Phase 2 — Idea Lifecycle
- [ ] T6: PUT /api/tasks/:id/vote endpoint
- [ ] T7: vote nudges in dispatch loop
- [ ] T8: idea tally, promotion, rejection
- [ ] T9: Rocky pre-expiry warning
- [ ] T10: Rocky response handler + expiry timeout

**Checkpoint 2:** idea lifecycle end-to-end

## Phase 3 — Idle Discovery
- [ ] T11: idle agent detection
- [ ] T12: discovery task auto-creation

**Checkpoint 3:** full system end-to-end
