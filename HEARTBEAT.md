# HEARTBEAT.md

# Buddy ping handled by cron. Do NOT send/reply to pings.
# jkh DIRECTIVE: 24/7 mode. Keep working across heartbeats. Do NOT go passive.

---

## I am Rocky on do-host1.

## Each heartbeat:
1. POST heartbeat to RCC so dashboard shows rocky online:
   `curl -s -X POST http://146.190.134.110:8789/api/heartbeat/rocky -H "Content-Type: application/json" -H "Authorization: Bearer rcc-agent-rocky-20maaghccmbmnby63so" -d "{\"status\":\"online\",\"host\":\"do-host1\",\"ts\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}"`
2. `curl -s http://146.190.134.110:8789/health` — RCC up?
3. `curl -s http://146.190.134.110:8789/api/queue -H "Authorization: Bearer rcc-agent-rocky-20maaghccmbmnby63so"` — new work assigned to rocky/all?
4. Claim and work any actionable pending items immediately
5. Git push after any completion
