#!/usr/bin/env bash
# run-coding-agent.sh — Thin wrapper that dispatches coding tasks via dispatch.mjs
#
# Usage: run-coding-agent.sh --repo <path> --prompt <text> [--executor <type>] [--model <model>]
#
# The executor is selected by dispatch.mjs in this order:
#   1. --executor flag (maps to preferred_executor)
#   2. PREFERRED_EXECUTOR env var
#   3. Agent default (claude_cli)
#
# Executor types: claude_cli | claude_sdk | codex_cli | codex_vllm | cursor_cli | opencode
#
# Exit codes: 0=success, 1=all backends failed, 2=usage error

set -euo pipefail

REPO=""
PROMPT=""
EXECUTOR="${PREFERRED_EXECUTOR:-}"
MODEL="${OPENCODE_MODEL:-}"
LOG_FILE="${CODING_AGENT_LOG:-/tmp/coding-agent.log}"

usage() {
  echo "Usage: $0 --repo <path> --prompt <text> [--executor <type>] [--model <model>]" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)     REPO="$2";     shift 2 ;;
    --prompt)   PROMPT="$2";   shift 2 ;;
    --executor) EXECUTOR="$2"; shift 2 ;;
    --model)    MODEL="$2";    shift 2 ;;
    *) usage ;;
  esac
done

[[ -z "$REPO"   ]] && usage
[[ -z "$PROMPT" ]] && usage

log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" | tee -a "$LOG_FILE"; }

# Locate dispatch.mjs — check executor dir relative to this script, then ~/.ccc/executors
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DISPATCH="${SCRIPT_DIR}/../executors/dispatch.mjs"
if [[ ! -f "$DISPATCH" ]]; then
  DISPATCH="$HOME/.ccc/executors/dispatch.mjs"
fi
if [[ ! -f "$DISPATCH" ]]; then
  log "dispatch.mjs not found — run 'cp -r ~/.ccc/workspace/workqueue/executors ~/.ccc/executors'"
  exit 1
fi

# Build item JSON
ITEM_JSON=$(python3 -c "
import json, sys
item = {
    'id':          'adhoc-' + __import__('time').strftime('%Y%m%dT%H%M%S'),
    'title':       'Ad-hoc coding task',
    'description': sys.argv[1],
    'repoPath':    sys.argv[2],
}
if sys.argv[3]: item['preferred_executor'] = sys.argv[3]
print(json.dumps(item))
" "$PROMPT" "$REPO" "$EXECUTOR")

# Build agent config JSON
CONFIG_JSON=$(python3 -c "
import json, sys
cfg = {'repoPath': sys.argv[1]}
if sys.argv[2]: cfg['model'] = sys.argv[2]
print(json.dumps(cfg))
" "$REPO" "$MODEL")

log "Dispatching task via dispatch.mjs (executor=${EXECUTOR:-auto}, repo=$REPO)"

# Run dispatcher — node must be available
if ! command -v node &>/dev/null; then
  log "node not found — cannot run dispatch.mjs"
  exit 1
fi

(cd "$(dirname "$DISPATCH")" && \
  node "$(basename "$DISPATCH")" --item "$ITEM_JSON" --config "$CONFIG_JSON") | tee -a "$LOG_FILE"
