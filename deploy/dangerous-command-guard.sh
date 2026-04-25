#!/usr/bin/env bash
# dangerous-command-guard.sh — PreToolUse safety hook for agent environments
#
# Usage (sourced):
#   source deploy/dangerous-command-guard.sh
#   guard_check "rm -rf /tmp/foo"   # returns 0=safe, 1=blocked/needs-verify, 2=always-blocked
#
# Usage (standalone):
#   bash deploy/dangerous-command-guard.sh "rm -rf /tmp/foo"
#
# GUARD_LEVEL=1 (default): block high-risk commands, print warning, exit 1
# GUARD_LEVEL=2 (autonomous): self-verification prompt for high-risk, exit 1
#
# Exit codes:
#   0 — safe to proceed
#   1 — blocked (level 1) or self-verify required (level 2)
#   2 — always blocked regardless of level

GUARD_LEVEL="${GUARD_LEVEL:-1}"

# ── helpers ──────────────────────────────────────────────────────────────────

_guard_always_block() {
    local cmd="$1"
    local reason="$2"
    echo "[GUARD] ALWAYS-BLOCKED: ${reason}" >&2
    echo "[GUARD] Command: ${cmd}" >&2
    echo "[GUARD] This command is unconditionally prohibited regardless of GUARD_LEVEL." >&2
    return 2
}

_guard_level1_block() {
    local cmd="$1"
    local reason="$2"
    echo "[GUARD] BLOCKED (GUARD_LEVEL=1): ${reason}" >&2
    echo "[GUARD] Command: ${cmd}" >&2
    echo "[GUARD] Set GUARD_LEVEL=2 for autonomous self-verification mode." >&2
    return 1
}

_guard_level2_verify() {
    local cmd="$1"
    local reason="$2"
    echo "[GUARD] SELF-VERIFY REQUIRED (GUARD_LEVEL=2): ${reason}" >&2
    echo "[GUARD] Command: ${cmd}" >&2
    echo "" >&2
    echo "[GUARD] Before proceeding, reason through each of the following:" >&2
    echo "  1. Is this command necessary to achieve the stated goal?" >&2
    echo "  2. What are the consequences if this command is wrong or runs on the wrong target?" >&2
    echo "  3. Is there a safer alternative that achieves the same outcome?" >&2
    echo "  4. Is this action aligned with the user's intent and scope of the task?" >&2
    echo "" >&2
    echo "[GUARD] Do NOT proceed unless you can answer all four questions confidently." >&2
    echo "[GUARD] Return exit code 1 — do not execute this command automatically." >&2
    return 1
}

# ── pattern matchers ──────────────────────────────────────────────────────────

_guard_matches_always_block() {
    local cmd="$1"

    # rm -rf / or rm -rf /*
    if echo "$cmd" | grep -qE 'rm\s+(-[a-zA-Z]*r[a-zA-Z]*f|-[a-zA-Z]*f[a-zA-Z]*r)\s+/\s*$'; then
        _guard_always_block "$cmd" "rm -rf / (root filesystem wipe)"; return 2
    fi
    if echo "$cmd" | grep -qE 'rm\s+(-[a-zA-Z]*r[a-zA-Z]*f|-[a-zA-Z]*f[a-zA-Z]*r)\s+/\*'; then
        _guard_always_block "$cmd" "rm -rf /* (root filesystem wipe)"; return 2
    fi

    # mkfs (any variant)
    if echo "$cmd" | grep -qE '\bmkfs(\.[a-z0-9]+)?\b'; then
        _guard_always_block "$cmd" "mkfs — filesystem creation/destruction"; return 2
    fi

    # dd if= (disk writes)
    if echo "$cmd" | grep -qE '\bdd\b.*\bif='; then
        _guard_always_block "$cmd" "dd if= — raw disk write"; return 2
    fi

    # fork bomb :(){ :|:& };:
    if echo "$cmd" | grep -qE ':\(\)\s*\{.*:\|.*:.*&.*\}'; then
        _guard_always_block "$cmd" "fork bomb detected"; return 2
    fi

    # --accept-data-loss
    if echo "$cmd" | grep -q -- '--accept-data-loss'; then
        _guard_always_block "$cmd" "--accept-data-loss flag"; return 2
    fi

    # prisma migrate reset
    if echo "$cmd" | grep -qE '\bprisma\b.*\bmigrate\b.*\breset\b'; then
        _guard_always_block "$cmd" "prisma migrate reset — destroys database"; return 2
    fi

    # DROP DATABASE or DROP TABLE (case-insensitive)
    if echo "$cmd" | grep -qiE '\bDROP\s+(DATABASE|TABLE)\b'; then
        _guard_always_block "$cmd" "DROP DATABASE / DROP TABLE — destructive DDL"; return 2
    fi

    # shutdown / reboot / poweroff without explicit safe flags
    if echo "$cmd" | grep -qE '\b(shutdown|reboot|poweroff|halt)\b'; then
        # Allow only if followed by safe/cancel flags like -c (cancel) or --no-wall
        if ! echo "$cmd" | grep -qE '\b(shutdown|reboot|poweroff|halt)\b.*-c\b'; then
            _guard_always_block "$cmd" "shutdown/reboot/poweroff — system termination"; return 2
        fi
    fi

    return 0
}

_guard_matches_high_risk() {
    local cmd="$1"

    # rm -rf <any path>
    if echo "$cmd" | grep -qE 'rm\s+(-[a-zA-Z]*r[a-zA-Z]*f|-[a-zA-Z]*f[a-zA-Z]*r)\s+\S+'; then
        echo "rm -rf — recursive forced removal"; return 1
    fi

    # chmod -R 777
    if echo "$cmd" | grep -qE '\bchmod\b.*-[a-zA-Z]*R[a-zA-Z]*\b.*777\b'; then
        echo "chmod -R 777 — world-writable recursive permission change"; return 1
    fi
    if echo "$cmd" | grep -qE '\bchmod\b.*777\b.*-[a-zA-Z]*R[a-zA-Z]*\b'; then
        echo "chmod -R 777 — world-writable recursive permission change"; return 1
    fi

    # git push --force to main or master
    if echo "$cmd" | grep -qE '\bgit\b.*\bpush\b.*(--force|-f)\b' && \
       echo "$cmd" | grep -qE '\b(main|master)\b'; then
        echo "git push --force to main/master — rewrites shared history"; return 1
    fi

    # curl/wget piped to bash or sh
    if echo "$cmd" | grep -qE '\b(curl|wget)\b.*\|\s*(bash|sh)\b'; then
        echo "curl/wget | bash — remote code execution without inspection"; return 1
    fi

    # writes to sensitive system paths
    if echo "$cmd" | grep -qE '(>|>>|tee|cp|mv|install|ln)\s.*\s/(etc|proc|sys|boot)/'; then
        echo "write to /etc/ /proc/ /sys/ /boot/ — sensitive system path"; return 1
    fi
    if echo "$cmd" | grep -qE '\s/(etc|proc|sys|boot)/\S+'; then
        # Only flag if it's a write-like context (not a simple read/cat)
        if echo "$cmd" | grep -qE '^\s*(echo|printf|tee|install|cp|mv|sed -i|awk.*>)'; then
            echo "write to sensitive system path"; return 1
        fi
    fi

    return 0
}

# ── main guard function ───────────────────────────────────────────────────────

guard_check() {
    local cmd="$1"

    if [[ -z "$cmd" ]]; then
        echo "[GUARD] No command provided." >&2
        return 0
    fi

    # Always-block check (exit 2)
    _guard_matches_always_block "$cmd"
    local always_rc=$?
    if [[ $always_rc -eq 2 ]]; then
        return 2
    fi

    # High-risk check (exit 1, behavior depends on GUARD_LEVEL)
    local reason
    reason=$(_guard_matches_high_risk "$cmd")
    local high_rc=$?

    if [[ $high_rc -eq 1 ]]; then
        if [[ "$GUARD_LEVEL" -ge 2 ]]; then
            _guard_level2_verify "$cmd" "$reason"
        else
            _guard_level1_block "$cmd" "$reason"
        fi
        return 1
    fi

    return 0
}

# ── standalone entrypoint ─────────────────────────────────────────────────────

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    if [[ $# -eq 0 ]]; then
        echo "Usage: $0 <command-string>" >&2
        echo "       GUARD_LEVEL=2 $0 <command-string>" >&2
        exit 1
    fi
    guard_check "$*"
    exit $?
fi
