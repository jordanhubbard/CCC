# Workqueue Idea Voting Protocol

_Implemented: 2026-03-19 (wq-20260319-009). Any agent can vote._

## How It Works

Ideas accumulate in the queue with `priority: "idea"`. Without a promotion path, they just sit there. This protocol gives them a path to real work.

## Voting

Any agent (or jkh) can cast a vote by adding their name to the `votes[]` array on an item.

```json
{
  "id": "wq-20260319-016",
  "priority": "idea",
  "votes": ["rocky", "natasha"]
}
```

**To vote:** In your next WORKQUEUE_SYNC, include the item with your name added to `votes[]` and `itemVersion` incremented. Other agents merge by highest itemVersion (or union of votes arrays if concurrent).

## Promotion Criteria

An idea is promoted to `priority: "normal"` when **either**:

1. **`"jkh"` is in `votes[]`** — jkh's single vote is always sufficient
2. **`votes.length >= 2`** AND at least one vote is from an agent other than the item's `source`

On promotion:
- `priority` → `"normal"`
- `status` → `"pending"` (if it was blocked/deferred for vote-related reasons)
- `itemVersion` incremented
- A note added with timestamp + reason

## Running the Check

```bash
# Check and promote (runs against local queue.json):
node workqueue/scripts/idea-promotion-check.mjs

# Dry run (shows what would be promoted, no writes):
node workqueue/scripts/idea-promotion-check.mjs --dry-run

# Against a specific queue file:
node workqueue/scripts/idea-promotion-check.mjs /path/to/queue.json
```

Designed to be called at the end of each workqueue cron cycle, after sync.

## Current Idea Backlog

| ID | Title | Votes | Status |
|----|-------|-------|--------|
| wq-20260319-014 | Overnight render queue | (none) | needs votes |
| wq-20260319-015 | Quiet hours protocol | (none) | blocked |
| wq-20260319-016 | Session continuity tracker | (none) | needs votes |

To move any of these forward: add your name to `votes[]` in your next sync.
