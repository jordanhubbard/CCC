# Work Queue — Rocky & Bullwinkle & Natasha

## Overview

A shared, persistent work queue system for the three agents (Bullwinkle, Rocky, Natasha).
Each agent maintains a local copy of the queue and syncs with peers when channels are available.

## Design Principles

1. **Lossy-tolerant**: Comms can be intermittent. The queue persists locally and syncs opportunistically.
2. **Cron-driven**: A periodic cron job processes the queue — not dependent on active conversations.
3. **Multi-channel fallback**: Mattermost → Slack → peer-to-peer gateway → Google Drive.
4. **Self-generating**: Agents can inject improvement ideas when idle.
5. **DM-driven**: Slack DMs requesting work become queue items automatically.

## File Format

Each agent keeps a local `queue.json`:

```json
{
  "version": 1,
  "agent": "bullwinkle",
  "lastSync": "2026-03-18T01:30:00.000Z",
  "items": [
    {
      "id": "wq-20260318-001",
      "itemVersion": 1,
      "created": "2026-03-18T01:00:00.000Z",
      "source": "rocky",
      "assignee": "bullwinkle",
      "priority": "normal",
      "status": "pending",
      "title": "Review and update MEMORY.md",
      "description": "Consolidate recent daily notes into long-term memory",
      "channel": "mattermost",
      "tags": ["maintenance", "memory"],
      "claimedBy": null,
      "claimedAt": null,
      "attempts": 0,
      "maxAttempts": 3,
      "lastAttempt": null,
      "completedAt": null,
      "result": null
    }
  ],
  "completed": [],
  "syncLog": []
}
```

## Item Lifecycle

```
pending → in_progress → completed | failed | deferred
```

- **pending**: Ready to be picked up
- **in_progress**: Currently being worked on
- **completed**: Done, with result summary
- **failed**: Exceeded maxAttempts or unrecoverable error
- **deferred**: Blocked, needs human input or external dependency

## Claim Mechanism (Optimistic Locking)

Before processing an item, set `claimedBy = <agent>` and `claimedAt = <ISO-8601>`.
On sync, if you receive an item you're about to work on and it has someone else's
`claimedBy` with a newer `claimedAt` than yours, **back off** — they got it first.
No central coordinator needed; timestamps resolve conflicts.

## Item Versioning

Each item has an `itemVersion` field (integer, starts at 1). Increment on any
status change. This allows future spec migrations and conflict resolution —
on merge, prefer the higher `itemVersion`.

## Urgent Item Pings

Items with `priority: "urgent"` should NOT wait for the next cron tick.
When creating or receiving an urgent item, immediately send a direct Mattermost DM
to the assignee (outside the sync envelope) alerting them. Example:

```
🚨 URGENT WORK ITEM: [title] — assigned to you. Check your workqueue.
```

The cron still processes it normally; the ping just ensures fast response.

## Sync Protocol

When the cron fires:

1. Read local `queue.json`
2. Process any `pending` items assigned to this agent
3. Try to reach peers (in fallback order):
   a. Mattermost DM — send/receive sync messages
   b. Slack DM — fallback
   c. Peer-to-peer gateway HTTP — fallback
   d. Google Drive `handoffs/incoming/` — last resort
4. Merge incoming items (dedup by `id`)
5. Share completed items back to peers
6. If idle and no pending items, optionally generate improvement ideas

## Sync Message Format

Sync messages are JSON payloads wrapped in a recognizable envelope:

```
🔄 WORKQUEUE_SYNC
{"from":"bullwinkle","itemCount":3,"items":[...],"completed":[...],"ts":"ISO-8601"}
```

The `itemCount` field lets the receiver sanity-check without fully parsing.
This lets agents distinguish sync messages from regular chat.

## Priority Levels

- **urgent**: Process immediately, alert human if needed
- **high**: Process in next cron tick
- **normal**: Process when convenient
- **low**: Background/improvement work, process when idle
- **idea**: Self-generated improvement suggestion, needs peer review before becoming work

## Tags

Common tags for categorization:
- `maintenance` — memory cleanup, file organization
- `skill` — skill improvement or creation
- `infrastructure` — config, comms, monitoring
- `content` — news, presentations, creative work
- `review` — peer review requested
- `human-request` — originated from jkh or other human DM

## Cron Schedule

Each agent runs the workqueue processor every 30 minutes, **staggered** to reduce sync noise:

- **Bullwinkle:** `:15` and `:45` past the hour
- **Rocky:** `:00` and `:30` past the hour
- **Natasha:** `:07` and `:37` past the hour

The processor is a cron job with `sessionTarget: "isolated"` to avoid polluting the main session.
