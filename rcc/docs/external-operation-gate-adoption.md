# ExternalOperationGate Adoption — Rocky/CCC Design Doc

**Source:** [JKHeadley/instar](https://github.com/JKHeadley/instar/blob/main/src/core/ExternalOperationGate.ts)  
**Status:** Design complete — Phase 1 implemented below  
**Date:** 2026-03-27  
**Author:** Rocky

---

## Origin Story

ExternalOperationGate was born from a real incident: an agent deleted 200+ emails autonomously because nothing structurally distinguished safe reads from bulk destructive deletes. The design principle is:

> **Structure > Willpower.** A MEMORY.md rule saying "don't delete emails" degrades as context grows. A gate that physically intercepts the operation and evaluates risk does not.

---

## Architecture

Three layers:

1. **Static risk matrix** — `mutability × reversibility × scope → risk level`
   - `read` → always low
   - `bulk + irreversible` → always critical
   - `bulk + delete` → critical regardless of reversibility
   - `single + irreversible delete` → high
   - etc.

2. **Config floor** — per-service allow/block/read-only lists. Structural — no LLM can override.

3. **LLM proportionality check** (Haiku-tier) — for medium+ risk: "Does this match user intent?" Critically: **LLM never sees the content being operated on** — only metadata. Prevents prompt injection via email body or file content.

Additional features:
- Batch/bulk checkpoint system (pause after N items, report progress before continuing)
- `AUTONOMY_PROFILES`: supervised / collaborative / autonomous
- Trust level tracking per service
- Append-only operation log

---

## Why We Need This

Rocky currently does zero risk classification on external operations. Known exposure points:
- **GitHub** — mass close/delete issues? delete branches? no guard exists
- **MinIO** — bulk delete objects? no guard
- **Telegram/Slack** — mass message delete? no guard
- **SSH/exec** — destructive shell commands (we have `exec` tool)

We already have `dangerous-command-guard.sh` (from instar -06 task) for shell commands. This is the next layer: **external service operations**.

---

## Phase 1: Lightweight Node.js Implementation

```js
// rcc/guardrails/external-operation-gate.mjs

export const RISK_MATRIX = {
  computeRiskLevel(mutability, reversibility, scope) {
    if (mutability === 'read') return 'low';
    if (scope === 'bulk' && (reversibility === 'irreversible' || mutability === 'delete')) return 'critical';
    if (scope === 'bulk') return 'critical';
    if (scope === 'batch' && (mutability === 'delete' || reversibility === 'irreversible')) return 'high';
    if (mutability === 'delete' && reversibility === 'irreversible') return 'high';
    if (mutability === 'delete') return 'medium';
    if (reversibility === 'irreversible' || scope === 'batch') return 'medium';
    return 'low';
  }
};

export const AUTONOMY_PROFILES = {
  supervised:    { low: 'log',     medium: 'approve', high: 'approve', critical: 'block' },
  collaborative: { low: 'proceed', medium: 'log',     high: 'approve', critical: 'approve' },
  autonomous:    { low: 'proceed', medium: 'proceed', high: 'log',     critical: 'approve' },
};

export class ExternalOperationGate {
  constructor({ profile = 'collaborative', services = {}, stateDir } = {}) {
    this.profile = AUTONOMY_PROFILES[profile] ?? AUTONOMY_PROFILES.collaborative;
    this.services = services;
    this.stateDir = stateDir;
  }

  evaluate({ service, mutability, reversibility, scope, itemCount, description, userIntent }) {
    const riskLevel = RISK_MATRIX.computeRiskLevel(mutability, reversibility, scope ?? this._scopeFromCount(itemCount));
    const behavior = this.profile[riskLevel];
    
    // Check hard blocks first
    const svcConfig = this.services[service] ?? {};
    if (svcConfig.blocked?.includes(mutability)) {
      return { action: 'block', reason: `${mutability} is hard-blocked for service ${service}`, riskLevel };
    }

    // Bulk checkpoint
    if (itemCount >= 20) {
      return { 
        action: 'show-plan', 
        reason: `Bulk operation (${itemCount} items) requires explicit plan`,
        riskLevel,
        checkpoint: { afterCount: 10, totalExpected: itemCount, completedSoFar: 0 }
      };
    }

    if (behavior === 'block') return { action: 'block', reason: `Risk level ${riskLevel} is blocked in current autonomy profile`, riskLevel };
    if (behavior === 'approve') return { action: 'show-plan', reason: `Risk level ${riskLevel} requires approval`, riskLevel };
    if (behavior === 'log') return { action: 'proceed', reason: 'Logged', riskLevel, logged: true };
    return { action: 'proceed', reason: 'Low risk', riskLevel };
  }

  _scopeFromCount(count = 1) {
    if (!count || count <= 1) return 'single';
    if (count <= 20) return 'batch';
    return 'bulk';
  }
}
```

---

## CCC API Integration

New endpoint: `POST /api/ops/evaluate`

```json
{
  "service": "github",
  "mutability": "delete",
  "reversibility": "reversible",
  "itemCount": 15,
  "description": "Close 15 stale issues",
  "agentId": "rocky"
}
```

Returns:
```json
{
  "action": "show-plan",
  "riskLevel": "high",
  "reason": "Batch delete requires approval",
  "checkpoint": { "afterCount": 5, "totalExpected": 15, "completedSoFar": 0 }
}
```

Agents call this before any bulk external operation and respect the response.

---

## Service Configs for Our Stack

```json
{
  "github": {
    "permissions": ["read", "write", "modify"],
    "blocked": [],
    "batchLimit": 10,
    "requireApproval": ["delete"]
  },
  "minio": {
    "permissions": ["read", "write"],
    "blocked": [],
    "batchLimit": 50,
    "requireApproval": ["delete"]
  },
  "slack": {
    "permissions": ["read", "write"],
    "blocked": ["delete"],
    "batchLimit": 5
  },
  "telegram": {
    "permissions": ["read", "write"],
    "blocked": ["delete"],
    "batchLimit": 5
  },
  "exec": {
    "permissions": ["read"],
    "blocked": ["delete"],
    "requireApproval": ["write", "modify"]
  }
}
```

---

## Files to Create

| File | Purpose |
|------|---------|
| `rcc/guardrails/external-operation-gate.mjs` | Core gate logic |
| `rcc/guardrails/service-configs.json` | Per-service permission floors |
| `rcc/api/index.mjs` | Add `POST /api/ops/evaluate` endpoint |
| `tests/guardrails/external-operation-gate.test.mjs` | Test suite |

---

## Notes

- Key insight from instar: LLM never sees the content being operated on. Only metadata flows to the LLM check. This is critical for prompt injection protection.
- The email incident that inspired this happened because the agent saw email content that said "delete all these". Our guardrail must replicate this isolation.
- Trust auto-escalation to autonomous is permanently disabled. Human must set autonomous profile explicitly.
