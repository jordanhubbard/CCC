#!/usr/bin/env bash
# bus-listener.sh — Subscribe to ClawBus SSE stream and react to hub directives.
#
# Handles:
#   rcc.update  → run agent-pull.sh immediately (no waiting for the 10-min timer)
#   rcc.quench  → pause work for N minutes (writes ~/.ccc/quench until <ts>)
#   rcc.exec    → execute shell code and post result to /api/exec/{id}/result
#                 (replaces the broken ccc-agent listen binary which has a reqwest
#                 zero-timeout bug that causes immediate connection close)
#
# Designed to run as a long-lived daemon under supervisord or systemd.
# Reconnects automatically on disconnect or error.
#
# Usage (direct):  bash bus-listener.sh
# Supervisord:     Registered by bootstrap.sh as [program:ccc-bus-listener]

set -euo pipefail

CCC_DIR="${HOME}/.ccc"
ENV_FILE="${CCC_DIR}/.env"
LOG_FILE="${CCC_DIR}/logs/bus-listener.log"
QUENCH_FILE="${CCC_DIR}/quench"

# ── Load .env ──────────────────────────────────────────────────────────────────
if [[ -f "$ENV_FILE" ]]; then
  set -a; source "$ENV_FILE"; set +a
fi

AGENT_NAME="${AGENT_NAME:-unknown}"
CCC_URL="${CCC_URL:-}"

if [[ -z "$CCC_URL" ]]; then
  echo "[bus-listener] ERROR: CCC_URL not set — cannot connect to ClawBus" >&2
  exit 1
fi

# Strip trailing slash
CCC_URL="${CCC_URL%/}"

# Resolve the workspace (same logic as agent-pull.sh)
WORKSPACE="${CCC_DIR}/workspace"

mkdir -p "${CCC_DIR}/logs"

log() {
  echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] [${AGENT_NAME}] [bus-listener] $1" | tee -a "$LOG_FILE" >&2
}

# ── JSON field extractor (python3, no jq dependency) ──────────────────────────
_json_field() {
  # _json_field <json_string> <field>
  python3 -c "
import json, sys
try:
    d = json.loads(sys.argv[1])
    print(d.get(sys.argv[2], ''))
except Exception:
    pass
" "$1" "$2" 2>/dev/null || true
}

# ── Handlers ──────────────────────────────────────────────────────────────────
handle_rcc_update() {
  local body="$1"
  local component branch
  component=$(_json_field "$body" "component")
  branch=$(_json_field "$body" "branch")
  log "rcc.update received — component=${component:-workspace} branch=${branch:-main}"

  PULL_SCRIPT="${WORKSPACE}/deploy/agent-pull.sh"
  if [[ -x "$PULL_SCRIPT" ]]; then
    log "Running agent-pull.sh..."
    bash "$PULL_SCRIPT" >> "$LOG_FILE" 2>&1 && log "agent-pull.sh complete" \
      || log "WARNING: agent-pull.sh exited non-zero"
  else
    log "WARNING: agent-pull.sh not found at $PULL_SCRIPT — trying git pull directly"
    if [[ -d "${WORKSPACE}/.git" ]]; then
      git -C "$WORKSPACE" pull --ff-only origin 2>&1 | tee -a "$LOG_FILE" || \
        log "WARNING: git pull failed"
    fi
  fi
}

handle_rcc_exec() {
  local msg_json="$1"  # full message JSON (not just body, because we need seq/from)
  local body exec_id code mode timeout_ms timeout_sec

  body=$(_json_field "$msg_json" "body")
  exec_id=$(_json_field "$body" "execId")
  code=$(_json_field "$body" "code")
  mode=$(_json_field "$body" "mode")
  timeout_ms=$(_json_field "$body" "timeout_ms")
  timeout_ms="${timeout_ms:-30000}"
  timeout_sec=$(( timeout_ms / 1000 ))
  [[ "$timeout_sec" -lt 1 ]] && timeout_sec=30

  if [[ -z "$exec_id" || -z "$code" ]]; then
    log "rcc.exec: invalid envelope (missing execId or code) — skipping"
    return
  fi

  # Check targets array — only execute if we're in the list or targets=["all"]
  local target_ok=false
  target_ok=$(python3 -c "
import json, sys
try:
  body = json.loads(sys.argv[1])
  targets = body.get('targets', [])
  agent = sys.argv[2]
  if 'all' in targets or agent in targets:
    print('true')
  else:
    print('false')
except Exception:
  print('false')
" "$body" "$AGENT_NAME" 2>/dev/null || echo "false")

  if [[ "$target_ok" != "true" ]]; then
    return  # Not targeted at us — skip silently
  fi

  log "rcc.exec ${exec_id}: running (mode=${mode:-shell}, timeout=${timeout_sec}s)"

  # Execute in background so the SSE read loop isn't blocked
  (
    local output exit_code=0
    if [[ "$mode" == "shell" || -z "$mode" ]]; then
      output=$(timeout "$timeout_sec" /bin/sh -c "$code" 2>&1) || exit_code=$?
      [[ $exit_code -eq 124 ]] && output="${output}
[timed out after ${timeout_sec}s]"
    else
      output="Unsupported mode: ${mode}"
      exit_code=1
    fi

    # Build result JSON safely
    local result_json
    result_json=$(python3 -c "
import json, sys
print(json.dumps({
  'agent':     sys.argv[1],
  'output':    sys.argv[2],
  'exit_code': int(sys.argv[3])
}))
" "$AGENT_NAME" "$output" "$exit_code" 2>/dev/null) || \
    result_json="{\"agent\":\"${AGENT_NAME}\",\"output\":\"(encode error)\",\"exit_code\":${exit_code}}"

    local http_status
    http_status=$(curl -sf -o /dev/null -w '%{http_code}' --max-time 15 \
      -X POST "${CCC_URL}/api/exec/${exec_id}/result" \
      -H "Authorization: Bearer ${CCC_AGENT_TOKEN:-}" \
      -H "Content-Type: application/json" \
      -d "$result_json" 2>/dev/null || echo "000")

    if [[ "$http_status" == "200" ]]; then
      log "rcc.exec ${exec_id}: result posted (exit=${exit_code})"
    else
      log "rcc.exec ${exec_id}: result POST returned HTTP ${http_status}"
    fi
  ) &
  disown  # Detach so the subshell doesn't become a zombie
}

handle_rcc_quench() {
  local body="$1"
  local minutes reason
  minutes=$(_json_field "$body" "minutes")
  reason=$(_json_field "$body" "reason")
  minutes="${minutes:-5}"
  local until_ts
  until_ts=$(python3 -c "
from datetime import datetime, timezone, timedelta
print((datetime.now(timezone.utc) + timedelta(minutes=$minutes)).strftime('%Y-%m-%dT%H:%M:%SZ'))
" 2>/dev/null || date -u -d "+${minutes} minutes" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || echo "")
  log "rcc.quench: pausing for ${minutes} min until ${until_ts} — ${reason}"
  echo "$until_ts" > "$QUENCH_FILE"
}

# ── SSE stream processor ───────────────────────────────────────────────────────
process_stream() {
  local stream_url="${CCC_URL}/bus/stream"
  log "Connecting to SSE stream: $stream_url"

  # Accumulate SSE data lines (may span multiple "data:" prefixes for large payloads)
  local data_buf=""

  while IFS= read -r line || [[ -n "$line" ]]; do
    # SSE lines: "data: <json>", "id: <id>", "event: <type>", or blank (message boundary)
    if [[ "$line" == data:* ]]; then
      data_buf="${line#data: }"
    elif [[ -z "$line" && -n "$data_buf" ]]; then
      # Message boundary — process the buffered data
      local msg_type msg_to msg_body
      msg_type=$(_json_field "$data_buf" "type")
      msg_to=$(_json_field   "$data_buf" "to")
      msg_body=$(_json_field "$data_buf" "body")

      # Only handle messages directed to us or broadcast
      if [[ "$msg_to" == "all" || "$msg_to" == "$AGENT_NAME" ]]; then
        case "$msg_type" in
          rcc.update) handle_rcc_update "$msg_body" ;;
          rcc.quench) handle_rcc_quench "$msg_body" ;;
          rcc.exec)   handle_rcc_exec   "$data_buf" ;;  # pass full msg for targets check
          ping)
            log "ping received from $(_json_field "$data_buf" "from")"
            ;;
          heartbeat|text|queue_sync|memo|event|pong|handoff|blob|status-response)
            : # ignore silently
            ;;
          *)
            [[ -n "$msg_type" ]] && log "Unhandled message type: $msg_type (to=$msg_to)"
            ;;
        esac
      fi

      data_buf=""
    fi
  done < <(curl -sSN --max-time 3600 \
    -H "Accept: text/event-stream" \
    -H "Authorization: Bearer ${CCC_AGENT_TOKEN:-}" \
    "${stream_url}" 2>>"$LOG_FILE")
}

# ── Main loop ─────────────────────────────────────────────────────────────────
log "Starting ClawBus listener (agent=${AGENT_NAME}, hub=${CCC_URL})"

RETRY_DELAY=5
MAX_RETRY_DELAY=120

while true; do
  process_stream
  log "SSE stream disconnected — reconnecting in ${RETRY_DELAY}s"
  sleep "$RETRY_DELAY"
  # Exponential backoff, cap at 120s
  RETRY_DELAY=$(( RETRY_DELAY * 2 > MAX_RETRY_DELAY ? MAX_RETRY_DELAY : RETRY_DELAY * 2 ))
  # Reset backoff after successful long connection (process_stream ran > 60s means connected)
  RETRY_DELAY=5
done
