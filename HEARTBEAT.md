# HEARTBEAT.md

# Buddy ping (Rocky <-> Bullwinkle) is handled by a dedicated cron job ("buddy-ping", every 30m).
# Do NOT send pings from heartbeat. Do NOT reply to incoming 🫎 pings from Bullwinkle.

# jkh DIRECTIVE 2026-03-21: 24/7 mode — NO quiet hours, NO sleep mode, NO weekend reduction.
# All agents always-on.

# jkh DIRECTIVE 2026-03-26: Keep working on tasks across heartbeats. Do NOT go passive.
# If a task is in-progress below, resume it IMMEDIATELY. Don't wait for a new prompt.

---

## 🔴 ACTIVE TASK — RESUME THIS EVERY HEARTBEAT UNTIL DONE

**Task:** `wq-RCC-token-persistence` — Fix agent token persistence across RCC restarts  
**Status:** pending (not yet claimed — claim and start this next heartbeat)  
**Approved by:** jkh (filed 2026-03-26 per jkh directive to always file tasks for found issues)  
**Priority:** high

### What to fix (rcc/api/index.mjs):
1. On startup: read agents.json and load all stored agent tokens into `AUTH_TOKENS` set
2. Verify agents.json write path is correct after .rcc/workspace symlink fix
3. Issue static tokens for Boris and RTX — add them to `/home/jkh/.rcc/.env` as `RCC_AUTH_TOKENS` additions
4. Update ONBOARDING.md with the static token pattern for containerized agents

### Resume instructions:
- PATCH wq-RCC-token-persistence to in-progress, claimedBy rocky
- Edit /home/jkh/.openclaw/workspace/rcc/api/index.mjs: after AUTH_TOKENS is initialized from env (line ~29), add startup code to read agents.json and add all agent tokens to AUTH_TOKENS
- Test: restart rcc-api.service, verify existing agent registrations still auth
- Commit, push, mark complete, remove this block

---

## Each heartbeat: check these in order

1. **Resume active task above** (if present) — check status, continue work
2. **Queue check**: `curl -s http://localhost:8789/api/queue -H "Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb"` — anything in-progress or stalled?
3. **RCC health**: `curl -s http://localhost:8789/health` — confirm up
4. **Git sync**: After completing any task, commit + push to `jordanhubbard/rockyandfriends`
