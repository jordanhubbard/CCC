# HEARTBEAT.md

# Buddy ping handled by cron. Do NOT send/reply to pings.
# jkh DIRECTIVE: 24/7 mode. Keep working across heartbeats. Do NOT go passive.
# See DIRECTIVES.md for full shared directives.

---

## No active tasks. Queue clear for Rocky.

## Each heartbeat:
1. Check DIRECTIVES.md for active directives
2. `curl -s http://localhost:8789/health` — RCC up?
3. `curl -s http://localhost:8789/api/queue -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb"` — new work assigned to rocky/all?
4. Claim and work any actionable pending items immediately
5. Git push after any completion
