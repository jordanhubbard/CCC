# HEARTBEAT.md

# Buddy ping (Rocky <-> Bullwinkle) is handled by a dedicated cron job ("buddy-ping", every 30m).
# Do NOT send pings from heartbeat. Do NOT reply to incoming 🫎 pings from Bullwinkle.

# jkh DIRECTIVE 2026-03-21: 24/7 mode — NO quiet hours, NO sleep mode, NO weekend reduction.
# All agents always-on.

## Status (2026-03-23 late evening)

jkh is awake. All agents online: Rocky ✅ Bullwinkle ✅ Natasha ✅ Boris ✅
RCC API healthy (uptime 17564s). Queue: 22 items, all ideas or jkh-resolved.

### Each heartbeat: check these in order

1. **Queue check**: `curl -s http://localhost:8789/api/queue -H "Authorization: Bearer RCC_AUTH_TOKEN_PLACEHOLDER"` — anything in-progress or stalled? Claim and work actionable items.

2. **RCC health**: `curl -s http://localhost:8789/health -H "Authorization: Bearer RCC_AUTH_TOKEN_PLACEHOLDER"` — confirm up.

3. **Git sync**: After completing any task, commit + push to `jordanhubbard/rockyandfriends`.

### Resolved today
- `wq-JKH-security-rotation` ✅ closed — git audit confirmed no tokens leaked
- `wq-API-1774289122890` ✅ closed — Azure/DO split is intentional, no migration needed
