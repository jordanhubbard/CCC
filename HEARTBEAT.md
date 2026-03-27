# HEARTBEAT.md

# Buddy ping (Rocky <-> Bullwinkle) handled by cron job. Do NOT send/reply to pings.
# jkh DIRECTIVE: 24/7 mode. Keep working across heartbeats. Do NOT go passive.
# See DIRECTIVES.md for full shared directives.

---

## 🔴 ACTIVE TASKS — RESUME EVERY HEARTBEAT UNTIL DONE

### [1] wq-AGENT-history-002 + wq-SCOUT-001
**Session:** `keen-seaslug` (Claude Code, background)
**Log:** /tmp/agent-history-scout.log
**Resume:** `process action:poll sessionId:keen-seaslug` → if done, test endpoints, mark complete
**What's being built:**
- GET /api/agents/history/:name — heartbeat history
- GET /api/agents/status — all agents + gap_minutes
- POST /api/heartbeat/:name now returns pendingWork[]
- GET /api/scout/:name — pending work for agent

### [2] wq-IDEATION-001
**Session:** `marine-falcon` (Claude Code, background)
**Log:** /tmp/ideation.log
**Resume:** `process action:poll sessionId:marine-falcon` → if done, test endpoints, mark complete
**What's being built:**
- rcc/ideation/ideation.mjs — LLM-based idea generation
- POST /api/ideation/generate — generate + file ideas
- GET /api/ideation/pending — list ideas
- POST /api/ideation/:id/promote — promote idea to real work

### When both complete:
- Test all new endpoints against http://localhost:8789
- Restart rcc-api.service
- Commit + push
- Mark wq-AGENT-history-002, wq-SCOUT-001, wq-IDEATION-001 completed
- Remove this block

---

## Each heartbeat:
1. **Check active tasks above** — poll sessions, resume if needed
2. `curl -s http://localhost:8789/health` — RCC up?
3. `curl -s http://localhost:8789/api/queue -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb"` — new work?
4. Git push after any completion
