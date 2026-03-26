# HEARTBEAT.md

# Buddy ping (Rocky <-> Bullwinkle) is handled by a dedicated cron job ("buddy-ping", every 30m).
# Do NOT send pings from heartbeat. Do NOT reply to incoming 🫎 pings from Bullwinkle.

# jkh DIRECTIVE 2026-03-21: 24/7 mode — NO quiet hours, NO sleep mode, NO weekend reduction.
# All agents always-on.

## Status (2026-03-23 late evening)

jkh is awake. All agents online: Rocky ✅ Bullwinkle ✅ Natasha ✅ Boris ✅
RCC API healthy (uptime 17564s). Queue: 22 items, all ideas or jkh-resolved.

### Each heartbeat: check these in order

0. **Post Rocky heartbeat to RCC** (keeps agent status card green on dashboard):
   ```
   curl -s -X POST http://localhost:8789/api/heartbeat/rocky \
     -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb" \
     -H "Content-Type: application/json" \
     -d '{"agent":"rocky","host":"do-host1","status":"online","ts":"<ISO_NOW>","services":{"minio":"ok","searxng":"ok","rcc":"ok"}}'
   ```

1. **Queue check**: `curl -s http://localhost:8789/api/queue -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb"` — anything in-progress or stalled? Claim and work actionable items.

2. **RCC health**: `curl -s http://localhost:8789/health -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb"` — confirm up.

3. **Git sync**: After completing any task, commit + push to `jordanhubbard/rockyandfriends`.

### Open request tickets

```
curl -s 'http://localhost:8789/api/requests?owner=rocky&status=open,delegated' \
  -H 'Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb'
```

- If any tickets have **all delegations resolved** but `notifiedRequesterAt` is null → POST `/api/requests/:id/close` with resolution summary.
- If any tickets are **open >30min with no delegation** → message the requester with a status update.

### Resolved today
- `wq-JKH-security-rotation` ✅ closed — git audit confirmed no tokens leaked
- `wq-API-1774289122890` ✅ closed — Azure/DO split is intentional, no migration needed
