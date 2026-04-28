# ACC Workflow Runbook

This runbook covers the `/api/tasks` outcome workflow.

## Inspect An Outcome

Use the dashboard Tasks tab or query `/api/tasks?project_id=<project>`.

Check these fields:
- `outcome_id`: groups root, work, review, gap, join, and commit tasks.
- `workflow_role`: one of `work`, `review`, `gap`, `join`, or `commit`.
- `review_result`: `approved` work can proceed; `rejected` work blocks finalization.
- `finisher_agent`: the only agent allowed to claim a `commit` role task.
- `blocked_by`: join and commit readiness dependencies.

## Readiness Checklist

An outcome is ready to commit when:
- All `work` role tasks are `completed`.
- Each required work task has `review_result = approved`.
- All blocking `gap` role tasks are completed and not rejected.
- A `join` role task exists and is completed.
- Exactly one `commit` role task exists or is filed by the server after join completion.

## Common Failure Cases

Wrong finisher:
- Symptom: claim returns `409` with `error = wrong_finisher`.
- Action: have the listed `finisher_agent` claim the task, or manually update `finisher_agent` if reassignment is intentional.

Rejected review:
- Symptom: no commit task appears after join completion.
- Action: inspect work/gap tasks in the same `outcome_id`; rejected review results block commit filing.

Stuck join:
- Symptom: join task returns `423 blocked`.
- Action: inspect `blocked_by`; each blocker must be completed and not rejected.

Dirty workspace fallback paused:
- Symptom: dirty project does not create legacy `phase_commit`.
- Action: if using target workflow mode, file/complete outcome join gates instead. If using migration fallback, set `ACC_ENABLE_DIRTY_PHASE_COMMIT_FALLBACK=true`.

Commit push failure:
- Symptom: commit task completes with a failure summary and bus alert.
- Action: fix git remote or SSH credentials manually. ACC intentionally does not auto-create more durable work for infrastructure failures.

## Manual Reassignment

Finisher stickiness is intentional. Do not reassign because another agent is less loaded.

Manual reassignment is appropriate only when:
- The finisher is offline beyond the stale threshold.
- The selected session is dead or unauthenticated.
- A human explicitly decides to transfer finalization ownership.

Update the task metadata field `finisher_agent` on every task in the same `outcome_id`, or at minimum on the `commit` task before claiming.
