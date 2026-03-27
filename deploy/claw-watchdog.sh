#!/bin/bash
# claw-watchdog.sh — OpenClaw process watchdog for containerized agents (no systemd)
#
# Monitors an OpenClaw process and restarts it when it hangs or dies.
# Works in two modes:
#   docker  — uses `docker restart <container>` (requires Docker socket access)
#   process — kills and re-execs openclaw directly (pure container, no daemon)
#
# Usage:
#   MODE=docker   CONTAINER_NAME=boris  bash claw-watchdog.sh
#   MODE=process  OPENCLAW_CMD="openclaw start" bash claw-watchdog.sh
#
# Environment:
#   MODE              docker | process (default: process)
#   CONTAINER_NAME    docker container name/id (docker mode only)
#   OPENCLAW_CMD      full command to (re)start openclaw (process mode only)
#   OPENCLAW_PID_FILE path to openclaw pid file (process mode, optional)
#   OPENCLAW_HEALTH   health check URL (default: http://localhost:18789/health)
#   RCC_URL           RCC API base URL for heartbeat staleness check
#   RCC_AGENT_TOKEN   RCC auth token
#   AGENT_NAME        agent name (for Mattermost alerts and logs)
#   MATTERMOST_URL    Mattermost server URL
#   MATTERMOST_TOKEN  Mattermost bot token
#   MATTERMOST_CHANNEL  channel ID to post restart alerts to
#   CHECK_INTERVAL    seconds between health checks (default: 30)
#   HANG_THRESHOLD    seconds of unresponsiveness before restart (default: 120)
#   HEARTBEAT_STALE   seconds since last RCC heartbeat before hang declared (default: 600)
#   MAX_RESTARTS      max restarts in RESTART_WINDOW seconds before giving up (default: 5)
#   RESTART_WINDOW    window in seconds for MAX_RESTARTS (default: 3600)
#   LOG_FILE          log path (default: ~/.rcc/logs/watchdog.log)

set -euo pipefail

# ── Config ─────────────────────────────────────────────────────────────────
MODE="${MODE:-process}"
CONTAINER_NAME="${CONTAINER_NAME:-}"
OPENCLAW_CMD="${OPENCLAW_CMD:-openclaw start}"
OPENCLAW_PID_FILE="${OPENCLAW_PID_FILE:-}"
OPENCLAW_HEALTH="${OPENCLAW_HEALTH:-http://localhost:18789/health}"
RCC_URL="${RCC_URL:-}"
RCC_AGENT_TOKEN="${RCC_AGENT_TOKEN:-}"
AGENT_NAME="${AGENT_NAME:-agent}"
MATTERMOST_URL="${MATTERMOST_URL:-}"
MATTERMOST_TOKEN="${MATTERMOST_TOKEN:-}"
MATTERMOST_CHANNEL="${MATTERMOST_CHANNEL:-}"
CHECK_INTERVAL="${CHECK_INTERVAL:-30}"
HANG_THRESHOLD="${HANG_THRESHOLD:-120}"
HEARTBEAT_STALE="${HEARTBEAT_STALE:-600}"
MAX_RESTARTS="${MAX_RESTARTS:-5}"
RESTART_WINDOW="${RESTART_WINDOW:-3600}"
LOG_FILE="${LOG_FILE:-${HOME}/.rcc/logs/watchdog.log}"
ENV_FILE="${ENV_FILE:-${HOME}/.rcc/.env}"

# ── Load .env ───────────────────────────────────────────────────────────────
if [ -f "$ENV_FILE" ]; then
  set -a; source "$ENV_FILE"; set +a
fi

# ── State ───────────────────────────────────────────────────────────────────
consecutive_failures=0
restart_times=()   # timestamps of recent restarts

mkdir -p "$(dirname "$LOG_FILE")"

# ── Logging ─────────────────────────────────────────────────────────────────
log() {
  local level="$1"; shift
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] [$AGENT_NAME/watchdog] [$level] $*" | tee -a "$LOG_FILE"
}

# ── Mattermost alert ────────────────────────────────────────────────────────
mm_alert() {
  local msg="$1"
  [ -z "$MATTERMOST_URL" ] || [ -z "$MATTERMOST_TOKEN" ] || [ -z "$MATTERMOST_CHANNEL" ] && return 0
  curl -s -X POST "$MATTERMOST_URL/api/v4/posts" \
    -H "Authorization: Bearer $MATTERMOST_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"channel_id\":\"$MATTERMOST_CHANNEL\",\"message\":\"$msg\"}" \
    --max-time 10 >/dev/null 2>&1 || true
}

# ── Health check ─────────────────────────────────────────────────────────────
check_health() {
  local http_code
  http_code=$(curl -s -o /dev/null -w "%{http_code}" \
    "$OPENCLAW_HEALTH" --max-time 10 2>/dev/null) || true
  [ "$http_code" = "200" ]
}

# ── RCC heartbeat staleness check ────────────────────────────────────────────
check_rcc_heartbeat() {
  [ -z "$RCC_URL" ] && return 0  # skip if no RCC configured
  local data
  data=$(curl -s "$RCC_URL/api/heartbeats" --max-time 10 2>/dev/null) || return 0
  local ts
  ts=$(echo "$data" | python3 -c "
import json,sys,time
d=json.load(sys.stdin)
hb=d.get('$AGENT_NAME',{})
ts=hb.get('ts','')
if not ts: sys.exit(1)
age=time.time()-time.mktime(__import__('datetime').datetime.strptime(ts,'%Y-%m-%dT%H:%M:%SZ').timetuple())
print(int(age))
" 2>/dev/null) || return 0
  [ "$ts" -lt "$HEARTBEAT_STALE" ]
}

# ── Restart logic ────────────────────────────────────────────────────────────
do_restart() {
  local reason="$1"
  local now
  now=$(date +%s)

  # Prune old restart timestamps outside window
  local fresh=()
  for t in "${restart_times[@]:-}"; do
    [ $(( now - t )) -lt "$RESTART_WINDOW" ] && fresh+=("$t")
  done
  restart_times=("${fresh[@]:-}")

  # Flap guard
  if [ "${#restart_times[@]}" -ge "$MAX_RESTARTS" ]; then
    log "ERROR" "Hit $MAX_RESTARTS restarts in ${RESTART_WINDOW}s — backing off, NOT restarting"
    mm_alert "🚨 [$AGENT_NAME] Watchdog FLAP GUARD: $MAX_RESTARTS restarts in ${RESTART_WINDOW}s. Manual intervention needed. Reason: $reason"
    return 1
  fi

  log "WARN" "Restarting OpenClaw. Reason: $reason (restart #$(( ${#restart_times[@]} + 1 )))"
  mm_alert "⚠️ [$AGENT_NAME] Watchdog restarting OpenClaw — $reason (restart #$(( ${#restart_times[@]} + 1 ))/${MAX_RESTARTS})"

  if [ "$MODE" = "docker" ]; then
    if [ -z "$CONTAINER_NAME" ]; then
      log "ERROR" "MODE=docker but CONTAINER_NAME not set"
      return 1
    fi
    docker restart "$CONTAINER_NAME" && log "INFO" "docker restart $CONTAINER_NAME succeeded" \
      || { log "ERROR" "docker restart failed"; return 1; }

  else
    # process mode — find and kill openclaw, then re-exec
    local pid=""
    if [ -n "$OPENCLAW_PID_FILE" ] && [ -f "$OPENCLAW_PID_FILE" ]; then
      pid=$(cat "$OPENCLAW_PID_FILE" 2>/dev/null) || true
    fi
    if [ -z "$pid" ]; then
      pid=$(pgrep -f "openclaw" | head -1) || true
    fi

    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      log "INFO" "Killing openclaw PID $pid"
      kill -9 "$pid" 2>/dev/null || true
      sleep 2
    fi

    log "INFO" "Starting openclaw: $OPENCLAW_CMD"
    eval "$OPENCLAW_CMD" &
    disown
    log "INFO" "OpenClaw re-launched (background)"
  fi

  restart_times+=("$now")
  consecutive_failures=0
  sleep 5  # give it time to come up before next check
}

# ── Main loop ─────────────────────────────────────────────────────────────────
log "INFO" "Watchdog started (mode=$MODE, interval=${CHECK_INTERVAL}s, hang_threshold=${HANG_THRESHOLD}s)"
mm_alert "🐾 [$AGENT_NAME] Watchdog started (mode=$MODE)"

while true; do
  if check_health; then
    consecutive_failures=0
    log "DEBUG" "Health OK"
  else
    consecutive_failures=$(( consecutive_failures + 1 ))
    fail_seconds=$(( consecutive_failures * CHECK_INTERVAL ))
    log "WARN" "Health check failed (consecutive: $consecutive_failures, ${fail_seconds}s unresponsive)"

    if [ "$fail_seconds" -ge "$HANG_THRESHOLD" ]; then
      do_restart "unresponsive for ${fail_seconds}s" || true
    fi
  fi

  # Secondary: check RCC heartbeat staleness (catches hung-but-port-open cases)
  if ! check_rcc_heartbeat 2>/dev/null; then
    log "WARN" "RCC heartbeat stale (>${HEARTBEAT_STALE}s) — process may be hung"
    do_restart "RCC heartbeat stale (>${HEARTBEAT_STALE}s)" || true
  fi

  sleep "$CHECK_INTERVAL"
done
