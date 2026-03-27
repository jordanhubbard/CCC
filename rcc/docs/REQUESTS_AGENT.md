# Request Tickets — Agent Guide

**Auth:** `Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb`
**Base URL:** `http://localhost:8789`

---

## Core Rule

> Never silently complete delegated human-origin work. **Always update the ticket.**

If you receive a delegation (e.g., Rocky tells you to do X and gives you a `requestId`), you must resolve that delegation via the API when you finish, so the original requester gets notified.

---

## How to Resolve a Delegation

When Rocky delegates to you, you'll be told:
- The request ticket ID (e.g., `req-1774289122890`)
- The delegation index (e.g., `0` for the first delegation)

When your work is done:

```bash
curl -s -X PATCH http://localhost:8789/api/requests/req-<id>/delegations/<idx> \
  -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb" \
  -H "Content-Type: application/json" \
  -d '{"outcome": "Brief description of what you accomplished"}'
```

---

## How to Add a requestId When Creating Queue Items

If you create queue items on behalf of a delegated request, link them to the ticket:

```bash
curl -s -X POST http://localhost:8789/api/queue \
  -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb" \
  -H "Content-Type: application/json" \
  -d '{
    "title": "Your task title",
    "priority": "high",
    "assignee": "bullwinkle",
    "requestId": "req-<id>"
  }'
```

When the queue item is completed via `POST /api/complete/:id`, RCC will automatically resolve the matching delegation on the parent ticket.

---

## Checking Your Open Delegations

```bash
# All open/delegated tickets (adjust owner= as needed)
curl -s 'http://localhost:8789/api/requests?status=open,delegated' \
  -H 'Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb'
```

---

## Adding a Delegation (Rocky only — when handing off to another agent)

```bash
curl -s -X POST http://localhost:8789/api/requests/req-<id>/delegate \
  -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb" \
  -H "Content-Type: application/json" \
  -d '{"to": "bullwinkle", "summary": "What Bullwinkle needs to do", "queueItemId": "wq-<optional>"}'
```

---

## Status Flow

```
open → delegated → resolved → closed
```

- `open`: ticket created, no delegations yet
- `delegated`: at least one delegation added
- `resolved`: all delegations resolved (RCC sets this automatically)
- `closed`: requester has been notified (`notifiedRequesterAt` set)

Only close a ticket after notifying the requester. Use `POST /api/requests/:id/close` with a `resolution` summary.
