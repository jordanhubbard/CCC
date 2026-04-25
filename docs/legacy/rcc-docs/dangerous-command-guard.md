# dangerous-command-guard.sh

A bash safety hook that intercepts dangerous shell commands before execution.
Designed to be sourced as a **PreToolUse** guard in agent environments.

---

## Pattern

```
source deploy/dangerous-command-guard.sh
guard_check "<command string>"
# 0 = safe   1 = blocked / self-verify   2 = always-blocked
```

The guard runs before every `exec`-style tool call. If it returns non-zero,
the agent must **not** execute the command.

---

## Two Levels

Set `GUARD_LEVEL` in the environment before sourcing the script.

| Level | `GUARD_LEVEL` | Behavior on high-risk command |
|-------|---------------|-------------------------------|
| **1** | `1` (default) | Print a warning and return 1. Command is blocked. |
| **2** | `2` (autonomous) | Print a self-verification prompt and return 1. Agent must reason before retrying. |

### Level 1 — Default / Supervised

Intended for interactive or supervised pipelines where a human is watching.
High-risk commands are silently refused with a short error message.

```
[GUARD] BLOCKED (GUARD_LEVEL=1): rm -rf — recursive forced removal
[GUARD] Command: rm -rf /tmp/foo
[GUARD] Set GUARD_LEVEL=2 for autonomous self-verification mode.
```

### Level 2 — Autonomous

Intended for fully autonomous agents. Instead of a hard block, the guard
prints four reasoning questions the agent must answer before retrying:

1. Is this command necessary to achieve the stated goal?
2. What are the consequences if this command is wrong or runs on the wrong target?
3. Is there a safer alternative that achieves the same outcome?
4. Is this action aligned with the user's intent and scope of the task?

The command still returns exit code 1. The agent **must not auto-retry** without
working through the four questions and producing a satisfactory answer to each.

---

## Always-Block List

These commands are rejected unconditionally (exit code 2) regardless of
`GUARD_LEVEL`. There is no self-verification path for these.

| Pattern | Reason |
|---------|--------|
| `rm -rf /` or `rm -rf /*` | Root filesystem wipe |
| `mkfs.*` | Filesystem creation — destroys existing data |
| `dd if=` | Raw disk write |
| `:(){ :\|:& };:` | Fork bomb |
| `--accept-data-loss` | Explicit data-loss acknowledgement flag |
| `prisma migrate reset` | Drops and recreates the entire database |
| `DROP DATABASE` / `DROP TABLE` | Destructive DDL (case-insensitive) |
| `shutdown` / `reboot` / `poweroff` / `halt` (without `-c`) | System termination |

---

## High-Risk List

These patterns are blocked at Level 1 and trigger self-verification at Level 2.

| Pattern | Reason |
|---------|--------|
| `rm -rf <path>` | Recursive forced removal of any path |
| `chmod -R 777` | World-writable recursive permission change |
| `git push --force` to `main`/`master` | Rewrites shared branch history |
| `curl \| bash` or `wget \| sh` | Remote code execution without inspection |
| Writes to `/etc/`, `/proc/`, `/sys/`, `/boot/` | Sensitive system paths |

---

## Agent Integration

### PreToolUse hook (recommended)

Source the guard at the top of any script that invokes shell commands on behalf
of an agent:

```bash
source "$(dirname "$0")/../deploy/dangerous-command-guard.sh"

run_shell_command() {
    local cmd="$1"
    guard_check "$cmd" || return $?
    eval "$cmd"
}
```

### Claude Code hook

Add to `.claude/settings.json` to intercept all Bash tool calls:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "source deploy/dangerous-command-guard.sh && guard_check \"$CLAUDE_TOOL_INPUT_COMMAND\""
          }
        ]
      }
    ]
  }
}
```

### Standalone check

```bash
# Level 1 (default)
bash deploy/dangerous-command-guard.sh "rm -rf /tmp/build"

# Level 2 (autonomous agent)
GUARD_LEVEL=2 bash deploy/dangerous-command-guard.sh "rm -rf /tmp/build"
```

### Exit code contract

| Code | Meaning | Agent action |
|------|---------|--------------|
| `0` | Safe | Proceed with execution |
| `1` | Blocked or self-verify | Do **not** execute; review or reason through |
| `2` | Always-blocked | Do **not** execute under any circumstances |
