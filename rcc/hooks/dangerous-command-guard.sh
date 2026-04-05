#!/bin/bash
# dangerous-command-guard.sh — PreToolUse safety hook for CCC agents
#
# Adapted from instar's "Security Through Identity" model
# (https://github.com/JKHeadley/instar — dangerous-command-guard.sh)
# Original: Level 1/2 hook with self-verification for Claude Code.
#
# Adapted for Rocky & Friends agent ecosystem (2026-03-27):
#   - Works with OpenClaw + Claude Code on all agent hosts
#   - Config reads from ~/.rcc/.env (RCC_SAFETY_LEVEL) or agent's
#     .openclaw/openclaw.json (agents.hooks.safetyLevel)
#   - Identity context from the agent's IDENTITY.md or SOUL.md
#   - Always-block list includes our learned incidents
#   - Audit trail appended to ~/.rcc/logs/guard.jsonl
#
# Safety Levels:
#   Level 1 (default): Block risky commands, require user authorization.
#     → Safe starting point. Human stays in the loop.
#   Level 2 (autonomous): Inject self-verification prompt instead of blocking.
#     → Agent reasons about correctness before proceeding.
#     → Catastrophic commands are ALWAYS blocked regardless of level.
#
# Install:
#   Claude Code: Add to .claude/settings.json hooks.preToolUse
#   OpenClaw: Wire into elevated exec policy
#
# The Level 1→2 progression is the path to autonomy. Structure > willpower.

set -euo pipefail

INPUT="${1:-}"
if [ -z "$INPUT" ]; then
  exit 0
fi

# --- Determine agent identity and config paths ---
AGENT_NAME="${AGENT_NAME:-unknown}"
RCC_DIR="${HOME}/.rcc"
OPENCLAW_DIR="${HOME}/.openclaw"
WORKSPACE_DIR="${OPENCLAW_DIR}/workspace"
LOG_DIR="${RCC_DIR}/logs"
LOG_FILE="${LOG_DIR}/guard.jsonl"

mkdir -p "$LOG_DIR"

# --- Determine safety level ---
# Priority: RCC_SAFETY_LEVEL env var > .rcc/.env > default (1)
SAFETY_LEVEL="${RCC_SAFETY_LEVEL:-}"
if [ -z "$SAFETY_LEVEL" ] && [ -f "$RCC_DIR/.env" ]; then
  SAFETY_LEVEL=$(grep '^RCC_SAFETY_LEVEL=' "$RCC_DIR/.env" 2>/dev/null | cut -d'=' -f2 | tr -d '"' || echo "")
fi
if [ -z "$SAFETY_LEVEL" ] && [ -f "$OPENCLAW_DIR/openclaw.json" ]; then
  SAFETY_LEVEL=$(python3 -c "
import json
try:
    c = json.load(open('$OPENCLAW_DIR/openclaw.json'))
    print(c.get('agents',{}).get('hooks',{}).get('safetyLevel', 1))
except: print(1)
" 2>/dev/null || echo "1")
fi
SAFETY_LEVEL="${SAFETY_LEVEL:-1}"

# --- Audit logging ---
log_event() {
  local action="$1" pattern="$2" level="$3"
  local ts
  ts=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  printf '{"ts":"%s","agent":"%s","action":"%s","pattern":"%s","level":%s,"input_hash":"%s"}\n' \
    "$ts" "$AGENT_NAME" "$action" "$pattern" "$level" \
    "$(echo -n "$INPUT" | shasum -a 256 | cut -d' ' -f1 | head -c 16)" \
    >> "$LOG_FILE" 2>/dev/null || true
}

# --- ALWAYS blocked (regardless of safety level) ---
# Catastrophic, irreversible operations no self-check can undo.
ALWAYS_BLOCK_PATTERNS=(
  "rm -rf /"
  "rm -rf ~"
  "rm -rf \$HOME"
  "> /dev/sda"
  "> /dev/disk"
  "mkfs\."
  "dd if="
  ":(){:|:&};:"
  # Database schema destruction — flags/commands that bypass safety checks.
  # Learned from Portal production data loss 2026-02-22 (instar incident).
  "--accept-data-loss"
  "prisma migrate reset"
  # Agent infrastructure: never destroy the shared workspace or agent configs
  "rm -rf.*\.rcc"
  "rm -rf.*\.openclaw"
  "rm -rf.*rockyandfriends"
  # MinIO/S3 destructive operations on our shared bucket
  "mc rm.*--force.*agents/"
  "mc rb.*agents"
  # Docker nuclear options on agent hosts
  "docker system prune -a"
  "docker volume prune -f"
)

for pattern in "${ALWAYS_BLOCK_PATTERNS[@]}"; do
  if echo "$INPUT" | grep -qiF -- "$pattern"; then
    log_event "HARD_BLOCK" "$pattern" "$SAFETY_LEVEL"
    echo "BLOCKED: Catastrophic command detected: $pattern" >&2
    echo "This command is always blocked regardless of safety level." >&2
    echo "If you genuinely need to run this, the user (jkh) must execute it directly." >&2
    exit 2
  fi
done

# --- Risky commands: behavior depends on safety level ---
RISKY_PATTERNS=(
  # Filesystem destruction
  "rm -rf \."
  "rm -rf \*"
  # Git force operations
  "git push --force"
  "git push -f"
  "git reset --hard"
  "git clean -fd"
  # Database destructive operations
  "DROP TABLE"
  "DROP DATABASE"
  "TRUNCATE"
  "DELETE FROM.*WHERE"
  # Prisma schema operations against potential production
  "prisma db push"
  "prisma migrate deploy"
  # Service-level destructive ops
  "systemctl stop"
  "systemctl disable"
  "launchctl unload"
  "kill -9"
  "killall"
  "pkill"
  # Network security
  "ufw disable"
  "iptables -F"
  "iptables --flush"
  # Credential/secret operations
  "curl.*secrets.*DELETE"
  "curl.*-X DELETE.*api/"
  # CCC-specific: queue wipe, agent decommission
  "api/queue.*DELETE"
  "api/agents.*decommission"
  # OpenClaw config changes
  "openclaw gateway config.apply"
)

for pattern in "${RISKY_PATTERNS[@]}"; do
  if echo "$INPUT" | grep -qi "$pattern"; then
    if [ "$SAFETY_LEVEL" -eq 1 ]; then
      # Level 1: Block and require authorization
      log_event "SOFT_BLOCK" "$pattern" "$SAFETY_LEVEL"
      echo "BLOCKED: Potentially destructive command detected: $pattern" >&2
      echo "Authorization required: Ask the user whether to proceed with this operation." >&2
      echo "Once they confirm, YOU execute the command — never ask the user to run it themselves." >&2
      exit 2
    else
      # Level 2: Inject self-verification prompt (don't block)
      log_event "SELF_VERIFY" "$pattern" "$SAFETY_LEVEL"

      # Load agent identity for self-verification context
      AGENT_IDENTITY=""
      if [ -f "$WORKSPACE_DIR/IDENTITY.md" ]; then
        AGENT_IDENTITY=$(head -20 "$WORKSPACE_DIR/IDENTITY.md")
      elif [ -f "$WORKSPACE_DIR/SOUL.md" ]; then
        AGENT_IDENTITY=$(head -20 "$WORKSPACE_DIR/SOUL.md")
      fi

      VERIFICATION=$(cat <<VERIFY
{
  "decision": "approve",
  "additionalContext": "=== SELF-VERIFICATION REQUIRED ===\nA potentially destructive command was detected: $pattern\n\nBefore proceeding, verify:\n1. Is this command necessary for the current task?\n2. Have you considered the consequences if this goes wrong?\n3. Is there a safer alternative that achieves the same result?\n4. Does this align with your principles and the user's intent?\n\nYour identity:\n$AGENT_IDENTITY\n\nIf ALL checks pass, proceed. If ANY check fails, stop and reconsider.\n=== END SELF-VERIFICATION ==="
}
VERIFY
)
      echo "$VERIFICATION"
      exit 0
    fi
  fi
done

# Command is safe — allow
exit 0
