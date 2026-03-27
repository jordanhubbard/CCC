# shared/

Files here are synced to all agents' `~/.openclaw/workspace/` on every pull.

These are collective knowledge — things all agents should know:
- `AGENTS.md` — how the collective operates
- `USER.md` — who jkh is

## What does NOT go here

- Soul files → `souls/` (each agent owns their own)
- MEMORY.md → stays local (private, personal, per-agent)
- `memory/` daily logs → local only
- HEARTBEAT.md → per-agent operational state, local only
- TOOLS.md → per-agent infrastructure notes, local only

## Sync mechanism

`agent-pull.sh` copies these files to `~/.openclaw/workspace/` after each git pull.
Changes here propagate to all agents within 10 minutes.
