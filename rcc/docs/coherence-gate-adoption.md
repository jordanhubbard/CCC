# CoherenceGate Adoption — Rocky/RCC Design Doc

**Source:** [JKHeadley/instar](https://github.com/JKHeadley/instar/blob/main/src/core/CoherenceGate.ts)  
**Status:** Design complete — implementation deferred to wq-INSTAR-*-12 (hot-patch cycle)  
**Date:** 2026-03-27  
**Author:** Rocky

---

## What Is It?

CoherenceGate is a multi-layer response review pipeline that evaluates agent responses **before they reach the user**. It sits between the agent's generation step and the outbound send.

Architecture:
1. **Policy Enforcement Layer (PEL)** — deterministic hard blocks (regex, denylist, structural rules). Zero LLM cost.
2. **Gate Reviewer** — fast Haiku-tier triage: "does this response need full review?" Skip expensive review for trivial replies.
3. **9 Specialist Reviewers** (run in parallel):
   - `conversational-tone` — Is the tone appropriate for the channel/recipient?
   - `claim-provenance` — Are factual claims grounded or hallucinated?
   - `settling-detection` — Is the agent giving up without trying alternatives? ← **Most useful for us**
   - `context-completeness` — Did the agent answer the full question?
   - `capability-accuracy` — Is the agent falsely claiming inability?
   - `url-validity` — Are referenced URLs real?
   - `value-alignment` — Does the response align with operator values?
   - `information-leakage` — Is the response leaking private context?
   - `escalation-resolution` — Are escalation promises being tracked?
4. **15-row normative decision matrix** → PASS / WARN / BLOCK / retry / hold / fail-open
5. **Canary corpus** — periodic health checks on reviewers using known-bad messages
6. **Audit log** with DSAR-compliant deletion support

---

## Why We Care

Rocky currently has **no response review layer**. We send whatever the LLM generates. This creates:
- Risk of settling (giving up prematurely, claiming inability without trying)
- Risk of tone mismatches across channels (Telegram vs Slack vs Mattermost)
- Risk of stale/hallucinated claims in commit messages, PR descriptions
- No audit trail of "near-misses" where the model almost sent something bad

The two reviewers most likely to catch real bugs in our context:
- **settling-detection**: catches "I can't do X" when X is possible
- **capability-accuracy**: catches false inability claims in response to workqueue items

---

## Adoption Plan

### Phase 1 (lightweight, no TypeScript port needed)

Implement a simplified JS version as a RCC PostToolUse hook or outbound message wrapper:

```js
// rcc/guardrails/coherence-gate.mjs
// Stripped-down version: PEL only + settling-detection

const SETTLING_PATTERNS = [
  /I('m| am) (unable|not able) to/i,
  /I (can't|cannot) (access|fetch|read|write)/i,
  /I don't have (access|permission|the ability)/i,
  /Unfortunately,? I (can't|cannot)/i,
  /I'm afraid I (can't|cannot)/i,
  /I (lack|do not have) (the )?capability/i,
];

export function checkSettling(response) {
  return SETTLING_PATTERNS.some(p => p.test(response));
}
```

Wire this into `openclaw.json` as a PostToolUse hook on message sends:
- If settling pattern detected → log to `~/.rcc/logs/settling.jsonl`
- Don't block (too risky for now) — just surface in heartbeat

### Phase 2 (full integration)

Full port of CoherenceGate logic to Node.js ESM:
- `rcc/guardrails/coherence-gate.mjs`
- `rcc/guardrails/reviewers/` (parallel reviewer map)
- Hook: `POST /api/coherence/evaluate` endpoint for agents to submit draft responses

### What We Skip

- TypeScript types (we use JSDoc)
- The research agent trigger (we have no in-flight research agent spawner)
- Custom reviewer loader (overkill for now)
- Per-channel fail behavior (start with global)

---

## Integration Point

The cleanest integration for OpenClaw is as a **PostToolUse hook on the `message` tool**:

```json
// openclaw.json additions
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "message",
        "script": "~/.rcc/workspace/rcc/guardrails/settling-check.sh"
      }
    ]
  }
}
```

The script reads the tool result, checks for settling patterns, logs if found.

---

## Files to Create

| File | Purpose |
|------|---------|
| `rcc/guardrails/coherence-gate.mjs` | Full Phase 2 implementation |
| `rcc/guardrails/settling-check.sh` | Phase 1 shell hook (immediate) |
| `rcc/guardrails/pel-rules.json` | PEL hard-block rules |
| `tests/guardrails/coherence-gate.test.mjs` | Test suite |

---

## Notes

- settling-detection is the highest-ROI reviewer for our use case
- The canary corpus idea is excellent — seed with known-bad patterns from RCC incident history
- DSAR-compliant deletion in audit log: good to have, lower priority
