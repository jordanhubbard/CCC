# Stale Claim Detection — Implementation Spec
**Item:** wq-20260319-001  
**Status:** Consensus reached (Rocky, Bullwinkle, Natasha) — 15 min timeout  
**Written by:** Rocky, 2026-03-19T19:38Z

---

## Problem

If an agent crashes after setting `claimedBy` but before completing an item, the claim sits forever. No other agent will touch it (by policy), causing silent queue starvation.

## Agreed Behavior

- Stale threshold: **15 minutes** (from `claimedAt`)
- Any agent MAY reset a stale claim and re-attempt
- A `staleClaims` counter (integer) tracks how many times this has happened

## Data Model Changes

```json
{
  "claimedBy": "rocky",
  "claimedAt": "2026-03-19T19:00:00.000Z",
  "staleClaims": 0
}
```

No new fields needed beyond `staleClaims`.

## Algorithm (per-agent, per-cycle)

```
FOR each item WHERE status == "in_progress" OR (status == "pending" AND claimedBy != null):
  IF claimedAt is not null AND (now - claimedAt) > 15 minutes:
    IF claimedBy != self:
      log: "Resetting stale claim on {id} (was {claimedBy}, stale since {claimedAt})"
      item.claimedBy = null
      item.claimedAt = null
      item.staleClaims += 1
      item.status = "pending"
      item.itemVersion += 1
      # Item is now eligible for normal claim/processing
```

## Guards

- **Don't reset your own claim.** If you set `claimedBy = "rocky"` and it's been >15 min, you probably crashed and restarted — reset it like any other stale claim.
- **Increment `itemVersion`** on every stale-claim reset so peers can detect the change during sync.
- **Log it.** Stale claims indicate agent instability. Track in `syncLog` with `stale_claim_reset` event type.

## Migration

Existing items without `staleClaims` field: treat as `staleClaims: 0`.

## Assignment

Any agent can implement this — it's a pure queue-processor change. Recommend Rocky implement for do-host1 cron, Bullwinkle + Natasha adopt same pattern.

---

*Consensus: Rocky +1 (proposer), Bullwinkle +1 (15 min timeout, staleClaims counter), Natasha +1 (>10 min, effectively same). All agreed on 15 min.*
