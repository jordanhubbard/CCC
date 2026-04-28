# ACC Spec Index

This directory contains the current authoritative spec set for ACC consolidation work.

Read these documents in order.

---

## 1. Platform-Level Control Plane

### [cli-first-session-routing.md](/Users/jordanh/Src/ACC/docs/specs/cli-first-session-routing.md)
Purpose:
- define the durable task plane
- define executor/session routing
- define CLI-first coding execution

This is the base control-plane spec.

---

## 2. Workflow-Level Consolidation

### [hermes-dag-outcome-scheduling.md](/Users/jordanh/Src/ACC/docs/specs/hermes-dag-outcome-scheduling.md)
Purpose:
- define Hermes-on-tasks
- define single-DAG durable workflow behavior
- define cooperative work plus single-finisher commit ownership

This is the architectural bridge from the control plane to workflow semantics.

---

## 3. Authoritative Implementation Contract

### [outcome-workflow-implementation-spec.md](/Users/jordanh/Src/ACC/docs/specs/outcome-workflow-implementation-spec.md)
Purpose:
- define exact task fields
- define exact workflow roles
- define exact finisher, review, commit, and migration rules
- define the acceptance-test matrix

If another model is implementing behavior, this is the primary specification to execute from.

---

## 4. Execution Manifest

### [spec-execution-manifest.md](/Users/jordanh/Src/ACC/docs/specs/spec-execution-manifest.md)
Purpose:
- define the required implementation phases
- define per-phase file targets
- define test targets
- define sequencing and non-goals for each cut

If another model is planning code changes, this is the primary execution-order document.

---

## 5. Ancillary Specs

### [../workflow-runbook.md](/Users/jordanh/Src/ACC/docs/workflow-runbook.md)
Purpose:
- define operator inspection and recovery steps for outcome workflows
- document wrong-finisher, rejected-review, stuck-join, and commit-failure handling

### [github-beads-sync.md](/Users/jordanh/Src/ACC/docs/specs/github-beads-sync.md)
Purpose:
- define GitHub/Beads synchronization behavior

This is independent of the control-plane and workflow consolidation track.

---

## Source Of Truth Rules

1. When documents disagree, use:
   1. `outcome-workflow-implementation-spec.md`
   2. `spec-execution-manifest.md`
   3. `hermes-dag-outcome-scheduling.md`
   4. `cli-first-session-routing.md`
2. `tasks/plan.md` and `tasks/todo.md` are planning mirrors, not the primary source of technical truth.
3. No new durable behavior should be specified only in Beads or only in task lists.

---

## Intended Reader Modes

- Architecture review: start at `cli-first-session-routing.md`
- Workflow design review: start at `hermes-dag-outcome-scheduling.md`
- Direct implementation by another LLM: start at `outcome-workflow-implementation-spec.md`, then `spec-execution-manifest.md`
