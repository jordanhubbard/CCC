# CCC Hooks

Agent-side safety hooks for the Rocky & Friends ecosystem. These run locally on each agent host as PreToolUse hooks (Claude Code) or exec policy guards (OpenClaw).

## dangerous-command-guard.sh

A PreToolUse bash hook with two safety levels, adapted from [instar](https://github.com/JKHeadley/instar)'s "Security Through Identity" model.

### Safety Levels

| Level | Behavior | Use Case |
|-------|----------|----------|
| **1** (default) | Block risky commands, require user authorization | New agents, cautious mode |
| **2** (autonomous) | Inject self-verification prompt — agent reasons about correctness | Trusted agents, full autonomy |

Catastrophic commands (`rm -rf /`, `mkfs`, fork bombs, `--accept-data-loss`, etc.) are **always blocked** regardless of level.

### Configuration

Set safety level via any of (priority order):

1. `RCC_SAFETY_LEVEL` environment variable
2. `RCC_SAFETY_LEVEL=2` in `~/.rcc/.env`
3. `agents.hooks.safetyLevel` in `~/.openclaw/openclaw.json`
4. Default: `1`

### Installation

**Claude Code (per-project):**
```json
// .claude/settings.json
{
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Bash",
        "command": "~/.rcc/workspace/rcc/hooks/dangerous-command-guard.sh \"$INPUT\""
      }
    ]
  }
}
```

**All agents (via deploy/agent-pull.sh):**
The hook is installed to `~/.rcc/workspace/rcc/hooks/` on every `git pull`. Each agent's HEARTBEAT or startup should verify the hook is executable.

### Audit Trail

Every guard event is logged to `~/.rcc/logs/guard.jsonl`:
```json
{"ts":"2026-03-27T19:15:00Z","agent":"bullwinkle","action":"HARD_BLOCK","pattern":"rm -rf /","level":1,"input_hash":"a1b2c3d4e5f6g7h8"}
```

Actions: `HARD_BLOCK` (catastrophic, always), `SOFT_BLOCK` (risky, level 1), `SELF_VERIFY` (risky, level 2).

### Always-Block Patterns

These are blocked at any safety level — no self-verification override:
- `rm -rf /`, `rm -rf ~`, `rm -rf $HOME`
- `mkfs.*`, `dd if=`, fork bombs
- `--accept-data-loss`, `prisma migrate reset` (learned from Portal incident)
- Agent infrastructure destruction (`.rcc`, `.openclaw`, `rockyandfriends`)
- MinIO force-delete on shared bucket
- Docker nuclear options (`system prune -a`, `volume prune -f`)

### Risky Patterns (Level-Dependent)

At Level 1: blocked with auth prompt. At Level 2: self-verification injected.
- Git force push/reset/clean
- SQL destructive ops (DROP, TRUNCATE, DELETE FROM)
- Prisma schema push/deploy
- Service stop/disable/kill
- Firewall disable/flush
- CCC queue/agent destructive API calls
- OpenClaw config.apply

### Level 1 → Level 2 Progression

The progression from Level 1 to Level 2 is the path to full autonomy:
- Start all agents at Level 1 (default)
- After an agent demonstrates consistent safe behavior over time, promote to Level 2
- Level 2 agents still can't run catastrophic commands — they just get to self-verify risky ones
- Structure > willpower: the hook makes safety structural, not optional
