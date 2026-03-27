# RCC Request Tracking — Design Spec

**Status:** Proposed  
**Author:** Rocky  
**Date:** 2026-03-26  
**Motivation:** Two related failure modes observed in production:

1. **Transitive completion gap** — jkh asks Rocky to do X, Rocky delegates to Bullwinkle, Bullwinkle finishes, jkh is never notified unless he happens to be watching.
2. **Multi-window gap** — jkh asks Rocky something, Rocky says "on it," context resets, Rocky is now stateless and jkh has to poll.

Both are the same root problem: **no persistent loop between a human request and its resolution.**

---

## Core Concept: Request Tickets

A **request ticket** is a lightweight record that survives context resets and transitive agent hops. It lives in RCC (not in any agent's session memory) and has a single lifecycle rule:

> **A ticket is only closed when the original requester has been notified of the outcome.**

---

## Data Model

```json
{
  "id": "req-<timestamp>",
  "created": "<ISO timestamp>",
  "requester": {
    "type": "human",
    "id": "jkh",
    "channel": "telegram"
  },
  "summary": "Ping Bullwinkle about wire-up commit and add queue item",
  "status": "open | delegated | resolved | closed",
  "owner": "rocky",
  "delegations": [
    {
      "to": "bullwinkle",
      "at": "<timestamp>",
      "summary": "Wire up dashboard-v2-frontend and open PRs",
      "resolvedAt": "<timestamp>",
      "outcome": "5cc6190 committed, PR #2 open"
    }
  ],
  "resolution": null,
  "notifiedRequesterAt": null,
  "closedAt": null
}
```

Key fields:
- **requester** — who originated the request (human or agent). For human requesters, includes channel for notification delivery.
- **delegations** — chain of sub-tasks handed off to other agents. Each has its own resolved state.
- **status** — transitions: `open → delegated → resolved → closed`. `closed` requires `notifiedRequesterAt` to be set.
- **resolution** — final outcome summary, written by the last agent in the chain before notifying.

---

## Lifecycle

```
Human asks Rocky
      │
      ▼
Rocky opens ticket (status=open, owner=rocky)
      │
      ├─── Rocky resolves directly
      │         │
      │         ▼
      │    Rocky notifies human → ticket closed
      │
      └─── Rocky delegates to Bullwinkle
                │
                ▼
           Delegation record added (status=delegated)
                │
                ▼
           Bullwinkle resolves delegation
                │
                ▼
           Bullwinkle updates ticket via RCC API
           (marks delegation resolved, sets outcome)
                │
                ▼
           RCC triggers notification to original requester
           (Rocky sends message to jkh's channel)
                │
                ▼
           Ticket closed
```

---

## Agent Behavior

### On session boot (Rocky)
1. Query `GET /api/requests?owner=rocky&status=open,delegated`
2. For each open ticket: check if delegations are resolved but requester not yet notified → send notification + close ticket
3. For each open ticket with no delegation: resume or report current status to requester

### On task completion (any agent)
1. `PATCH /api/requests/:id/delegations/:idx` — mark resolved with outcome
2. If no pending delegations remain: trigger requester notification

### On receiving a human request (Rocky)
1. If the request is non-trivial (more than one step, or involves other agents): open a ticket immediately, before starting work
2. Include the ticket ID in any Mattermost/Slack messages to other agents so they can reference it

---

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/requests` | Open a new request ticket |
| `GET` | `/api/requests` | List tickets (filter: owner, status, requester) |
| `GET` | `/api/requests/:id` | Get ticket detail |
| `PATCH` | `/api/requests/:id` | Update ticket (status, resolution, etc.) |
| `POST` | `/api/requests/:id/delegate` | Add a delegation record |
| `PATCH` | `/api/requests/:id/delegations/:idx` | Resolve a delegation |
| `POST` | `/api/requests/:id/close` | Notify requester and close (sets notifiedRequesterAt) |

---

## Notification Rules

- **Human requester:** send via their preferred channel (telegram, slack, etc.) — short summary of outcome
- **Agent requester:** update via Mattermost DM or workqueue sync
- Notification is **mandatory before close** — no silent closes
- If notification delivery fails: ticket stays in `resolved` state, retried on next boot

---

## Integration Points

### Workqueue
- Queue items can reference a request ticket ID (`requestId` field)
- When a queue item completes, if it has a `requestId`, trigger delegation resolution on the parent ticket
- This gives us transitive closure "for free" if agents remember to link them

### Session Boot Check
- Rocky's heartbeat / session bootstrap checks for open request tickets before doing anything else
- Takes ~1 API call, negligible overhead

### Dashboard
- New tab: **Requests** — shows open loops, requester, owner, delegation chain, age
- Aged-out open tickets (>24h unresolved) highlighted in red

---

## What This Does NOT Do

- Does not replace the workqueue — queue items are still the unit of work; request tickets are the loop-closure layer on top
- Does not require all interactions to go through tickets — only multi-step or delegated requests
- Does not track every message — only requests where jkh is waiting for an outcome

---

## Open Questions

1. **Granularity:** Should every "on it" trigger a ticket, or only requests that explicitly involve delegation or multi-step work? Recommendation: agent judgment — if it takes more than one tool call, open a ticket.
2. **Persistence:** Store in `rcc/data/requests.json` (same pattern as queue) or new DB table? Recommendation: flat JSON file to start, migrate later.
3. **Agent adoption:** Rocky opens tickets; Bullwinkle and Natasha need to know how to resolve delegations. Needs a short addition to `WORKQUEUE_AGENT.md` or a new `REQUESTS_AGENT.md`.

---

## Implementation Order

1. `POST/GET /api/requests` — basic CRUD (1–2 hours)
2. Session boot check in Rocky's heartbeat (30 min)
3. Delegation resolution + requester notification (1–2 hours)
4. Workqueue `requestId` linkage (30 min)
5. Dashboard tab (1 hour)
6. Agent doc updates (30 min)

Total estimate: ~6 hours across agents.
