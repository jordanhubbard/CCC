#!/usr/bin/env bash
# restart-fleet.sh — restart acc-agent on every online agent in the
# fleet. Query the hub's /api/agents?online=true for the registry,
# SSH to each using registered ssh_user/ssh_host/ssh_port, pull the
# latest code, and run deploy/restart-agent.sh.
#
# Serial by default so a broken fix doesn't brick the whole fleet
# simultaneously. Set PARALLEL=true to restart all agents at once
# (faster but higher blast radius).
#
# Usage:
#   bash deploy/restart-fleet.sh               # serial
#   PARALLEL=true bash deploy/restart-fleet.sh # concurrent
set -euo pipefail

# Load env (ACC_URL, ACC_AGENT_TOKEN)
if [ -f "$HOME/.acc/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    source "$HOME/.acc/.env"
    set +a
fi

ACC_URL="${ACC_URL:-http://localhost:8789}"
TOKEN="${ACC_AGENT_TOKEN:-${ACC_TOKEN:-}}"
if [ -z "$TOKEN" ]; then
    echo "[restart-fleet] ERROR: no ACC_AGENT_TOKEN or ACC_TOKEN in env / ~/.acc/.env" >&2
    exit 1
fi

PARALLEL="${PARALLEL:-false}"

echo "[restart-fleet] Querying ${ACC_URL}/api/agents?online=true"
AGENTS_JSON=$(curl -sSf -H "Authorization: Bearer $TOKEN" "${ACC_URL}/api/agents?online=true")

# Extract rows: name\tuser\thost\tport
mapfile -t TARGETS < <(echo "$AGENTS_JSON" | jq -r '
  (.agents // .) |
  .[] |
  select(.ssh_user != null and .ssh_user != "" and .ssh_host != null and .ssh_host != "") |
  [(.name // "?"), .ssh_user, .ssh_host, (.ssh_port // 22)] |
  @tsv')

if [ "${#TARGETS[@]}" -eq 0 ]; then
    echo "[restart-fleet] No online agents with ssh_host populated" >&2
    exit 1
fi

echo "[restart-fleet] ${#TARGETS[@]} target(s):"
for row in "${TARGETS[@]}"; do
    IFS=$'\t' read -r name user host port <<< "$row"
    echo "  - ${name} (${user}@${host}:${port})"
done
echo ""

restart_one() {
    local name="$1" user="$2" host="$3" port="$4"
    echo "[restart-fleet] → ${name}: ssh ${user}@${host}:${port}"
    # -oBatchMode=yes: no interactive prompts, fail fast if keys aren't set up
    # -oStrictHostKeyChecking=accept-new: tolerant of first-time hosts without prompting
    # Reset --hard origin/main: fleet nodes track main exactly. No local
    # commits, no edits in-flight. If a human is hand-debugging on a
    # fleet node, that's not a supported workflow.
    if ssh -o ConnectTimeout=10 \
           -o BatchMode=yes \
           -o StrictHostKeyChecking=accept-new \
           -p "$port" \
           "${user}@${host}" \
           "cd ~/.acc/workspace && git fetch --quiet origin && git reset --hard --quiet origin/main && bash deploy/restart-agent.sh" \
           2>&1 | sed "s/^/  [${name}] /"; then
        echo "[restart-fleet] ✓ ${name}"
        return 0
    else
        echo "[restart-fleet] ✗ ${name}"
        return 1
    fi
}

FAILED=0
if [ "$PARALLEL" = "true" ]; then
    declare -a PIDS=()
    for row in "${TARGETS[@]}"; do
        IFS=$'\t' read -r name user host port <<< "$row"
        restart_one "$name" "$user" "$host" "$port" &
        PIDS+=($!)
    done
    for pid in "${PIDS[@]}"; do
        if ! wait "$pid"; then FAILED=$((FAILED+1)); fi
    done
else
    for row in "${TARGETS[@]}"; do
        IFS=$'\t' read -r name user host port <<< "$row"
        restart_one "$name" "$user" "$host" "$port" || FAILED=$((FAILED+1))
    done
fi

echo ""
if [ "$FAILED" -eq 0 ]; then
    echo "[restart-fleet] ✓ all ${#TARGETS[@]} agent(s) restarted"
else
    echo "[restart-fleet] ✗ ${FAILED}/${#TARGETS[@]} agent(s) failed to restart"
    exit 1
fi
