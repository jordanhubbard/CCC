# AdaptiveTrust

Per-agent × per-service × per-operation trust model for the CCC agent system.

## Trust levels

| Level  | Meaning                                    |
|--------|--------------------------------------------|
| `none` | Always block — requires explicit approval every time |
| `ask`  | Ask user before proceeding (default for any new service/op) |
| `auto` | Proceed without asking |

## Three change paths

### 1. Earned (streak tracking)
Every successful operation increments a streak counter. Streaks surface
suggestions to the user but **never auto-promote to `auto`**:

- **Streak ≥ 10** — `source` becomes `earned:streak`. Prompts to the user
  should note the streak: _"Rocky has done this 10 times successfully, auto-approve?"_
- **Streak ≥ 20** — `source` becomes `earned:suggest-auto`. Suggest the user
  call `grantTrust()` to elevate to `auto`.

A failure resets the streak to 0.

### 2. Granted (user action)
`grantTrust(agentName, service, operation, grantedBy)` — the **only** path to
`auto` level. Records the granting user's ID for audit purposes.

### 3. Revoked (incident or user action)
`revokeTrust(agentName, service, operation, reason)` or
`recordFailure(..., revoke=true)` drops level to `none` and records a reason
and timestamp. Post-revocation, trust starts at `none` (more restrictive than
the `ask` default) until explicitly re-granted.

## Safety floor

> Trust can **never** auto-escalate to `auto`. Only an explicit `grantTrust()`
> call can reach that level. This ensures a human is always in the loop for
> unrestricted autonomy.

## State

Each agent's profile is stored at `~/.rcc/trust/<agentName>.json`:

```json
{
  "agentName": "rocky",
  "updatedAt": "2026-03-27T12:00:00.000Z",
  "services": {
    "github": {
      "read": {
        "level": "auto",
        "source": "granted",
        "streak": 42,
        "totalOps": 55,
        "lastOp": "2026-03-27T11:59:00.000Z",
        "grantedBy": "jkh"
      },
      "delete": {
        "level": "none",
        "source": "revoked",
        "streak": 0,
        "totalOps": 3,
        "lastOp": "2026-03-26T08:00:00.000Z",
        "revokedAt": "2026-03-26T08:01:00.000Z",
        "revokedReason": "deleted wrong branch"
      }
    }
  }
}
```

## Workqueue routing

Operations in the workqueue can gate task assignment on trust level:

```js
import { getTrustLevel } from '../trust/adaptive-trust.mjs';

// Only assign GPU tasks to agents with auto trust for gpu.submit
function canAssign(agentName, task) {
  if (task.type === 'gpu.submit') {
    return getTrustLevel(agentName, 'gpu', 'submit') === 'auto';
  }
  return true;
}
```

The `ask` level maps naturally to a human-approval step in the workqueue:
check trust, surface the streak context to the approver, then either proceed
or route to a manual queue.

## API

```js
import {
  getTrustLevel,   // (agentName, service, op) => 'none'|'ask'|'auto'
  recordSuccess,   // (agentName, service, op) — increments streak
  recordFailure,   // (agentName, service, op, reason, revoke=false)
  grantTrust,      // (agentName, service, op, grantedBy) — only path to auto
  revokeTrust,     // (agentName, service, op, reason)
  getTrustProfile, // (agentName) => full JSON object
  summarizeTrust,  // (agentName) => human-readable string
} from './adaptive-trust.mjs';
```
