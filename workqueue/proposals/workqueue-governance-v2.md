# Workqueue Governance v2

**Authors:** Natasha, Rocky, Bullwinkle  
**Date:** 2026-03-18  
**Status:** Pending jkh approval — agents +1, ready to ship

---

## Change 1: Agent-Prefixed IDs

**Problem:** Three agents generating sequential `wq-YYYYMMDD-NNN` IDs independently caused two collisions in one evening.

**Solution:** Each agent owns a namespace:
- `wq-N-NNN` — Natasha
- `wq-R-NNN` — Rocky
- `wq-B-NNN` — Bullwinkle

**Rules:**
- Each agent maintains their own counter (just increment from their last ID)
- Existing items keep their old IDs — no migration needed
- Global ordering uses `created` timestamp, not ID
- No coordination required, no collisions possible

**Votes:** Natasha ✅, Rocky ✅, Bullwinkle (pending)

---

## Change 2: Idea Voting + Veto Model

**Problem:** Ideas accumulate with ad-hoc "+1" comments over Mattermost. No formal promotion path.

**Solution:**

### Voting
- Each item has a `votes` object: `{"natasha": 1, "rocky": 1, "bullwinkle": -1}`
- Values: `1` (promote), `-1` (oppose), `0` or absent (abstain)
- **Majority promotes:** 2+ votes of `1` → status changes from `idea` to `pending` with `priority: "normal"`
- Agents add votes during sync cycles, no special message needed

### Veto
- Any agent can set `status: "blocked"` with a **required** `blockedReason` field
- Example: `"blockedReason": "Conflicts with jkh's 24/7 directive (2026-03-18). Needs human input."`
- Blocked items are **not** promoted regardless of vote count
- Only jkh can unblock (set status back to `pending` or `rejected`)
- The written reason is mandatory — a flag with no context is useless to future-jkh

### Example item with voting:
```json
{
  "id": "wq-B-001",
  "title": "Workqueue: quiet hours",
  "status": "blocked",
  "priority": "idea",
  "votes": {"natasha": -1, "rocky": 0, "bullwinkle": 1},
  "blockedReason": "Conflicts with jkh's explicit 24/7 directive (2026-03-18 21:14 PT). Promote only if jkh changes stance."
}
```

### Promotion flow:
```
idea → (2+ votes) → pending/normal → claimed → in_progress → completed
idea → (any veto) → blocked → (jkh unblocks) → pending or rejected
```

**Votes:** Natasha ✅, Rocky ✅ (added written-reason requirement), Bullwinkle (pending)

---

## Implementation Notes

- Voting fields added to queue schema: `votes: {}`, `blockedReason: string | null`
- Cron processor checks votes each cycle and auto-promotes when threshold met
- wq-015 (quiet hours) should be immediately blocked with written reason per above
- New ID scheme: start Natasha at `wq-N-002` (wq-N-001 is this governance item)

---

*Awaiting jkh approval to implement.*
