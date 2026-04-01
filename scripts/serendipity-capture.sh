#!/bin/bash
# serendipity-capture.sh — Capture a serendipity finding from a sub-agent.
#
# Usage:
#   scripts/serendipity-capture.sh \
#     --title "Short description" \
#     --description "Full explanation" \
#     --category improvement \
#     --rationale "Why this matters" \
#     --readiness idea-only
#
# Categories: bug, improvement, feature, pattern, refactor, security
# Readiness: idea-only, partially-implemented, implementation-complete, tested
#
# When a sub-agent is doing focused work and notices something adjacent worth
# capturing — but pursuing it would break focus — use this script to capture
# the finding as a workqueue idea item without losing the thread.
#
# Adapted from instar (JKHeadley/instar) for the rockyandfriends/OpenClaw
# agent fleet. Key adaptation: posts to RCC workqueue API instead of writing
# local .instar/state/serendipity/ JSON files.
#
# Original: instar/src/templates/scripts/serendipity-capture.sh
#
# Safety: rate limit (5/session), secret scanning, field length limits.

set -euo pipefail

# --- Configuration ---
MAX_PER_SESSION=5
MAX_TITLE_LEN=120
MAX_DESC_LEN=2000
MAX_RATIONALE_LEN=1000
VALID_CATEGORIES="bug improvement feature pattern refactor security"
VALID_READINESS="idea-only partially-implemented implementation-complete tested"

# --- RCC config ---
RCC_URL="${RCC_URL:-https://api.example.com}"
RCC_TOKEN="${RCC_AGENT_TOKEN:-}"
if [ -z "$RCC_TOKEN" ]; then
  # Try sourcing from ~/.rcc/.env
  if [ -f "$HOME/.rcc/.env" ]; then
    # shellcheck disable=SC1090
    source "$HOME/.rcc/.env" 2>/dev/null || true
    RCC_TOKEN="${RCC_AGENT_TOKEN:-}"
  fi
fi
if [ -z "$RCC_TOKEN" ]; then
  echo "Error: RCC_AGENT_TOKEN not set. Cannot post to workqueue." >&2
  exit 1
fi

AGENT_NAME="${AGENT_NAME:-unknown}"

# --- Argument parsing ---
TITLE=""
DESCRIPTION=""
CATEGORY=""
RATIONALE=""
READINESS=""

while [[ $# -gt 0 ]]; do
  case $1 in
    --title)       TITLE="$2";       shift 2 ;;
    --description) DESCRIPTION="$2"; shift 2 ;;
    --category)    CATEGORY="$2";    shift 2 ;;
    --rationale)   RATIONALE="$2";   shift 2 ;;
    --readiness)   READINESS="$2";   shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

# --- Validate required fields ---
[ -z "$TITLE" ]       && { echo "Error: --title is required" >&2; exit 1; }
[ -z "$DESCRIPTION" ] && { echo "Error: --description is required" >&2; exit 1; }
[ -z "$CATEGORY" ]    && { echo "Error: --category is required" >&2; exit 1; }
[ -z "$RATIONALE" ]   && { echo "Error: --rationale is required" >&2; exit 1; }
[ -z "$READINESS" ]   && { echo "Error: --readiness is required" >&2; exit 1; }

# --- Validate category ---
VALID=false
for c in $VALID_CATEGORIES; do
  [ "$CATEGORY" = "$c" ] && VALID=true && break
done
[ "$VALID" = "false" ] && { echo "Error: --category must be one of: $VALID_CATEGORIES" >&2; exit 1; }

# --- Validate readiness ---
VALID=false
for r in $VALID_READINESS; do
  [ "$READINESS" = "$r" ] && VALID=true && break
done
[ "$VALID" = "false" ] && { echo "Error: --readiness must be one of: $VALID_READINESS" >&2; exit 1; }

# --- Validate field lengths ---
[ ${#TITLE} -gt $MAX_TITLE_LEN ]       && { echo "Error: --title exceeds $MAX_TITLE_LEN chars" >&2; exit 1; }
[ ${#DESCRIPTION} -gt $MAX_DESC_LEN ]  && { echo "Error: --description exceeds $MAX_DESC_LEN chars" >&2; exit 1; }
[ ${#RATIONALE} -gt $MAX_RATIONALE_LEN ] && { echo "Error: --rationale exceeds $MAX_RATIONALE_LEN chars" >&2; exit 1; }

# --- Secret scanning ---
SECRET_PATTERNS='(AKIA[0-9A-Z]{16}|sk-[a-zA-Z0-9]{20,}|ghp_[a-zA-Z0-9]{36}|glpat-[a-zA-Z0-9\-]{20}|xox[bpors]-[a-zA-Z0-9\-]{10,}|-----BEGIN (RSA |EC |DSA )?PRIVATE KEY-----|password\s*[:=]\s*["'"'"'][^"'"'"']{8,}|[a-zA-Z0-9+/]{40,}={1,2})'
ALL_TEXT="$TITLE $DESCRIPTION $RATIONALE"
if echo "$ALL_TEXT" | grep -qEi "$SECRET_PATTERNS" 2>/dev/null; then
  echo "Error: Potential secret/credential detected in finding text. NOT captured." >&2
  exit 1
fi

# --- Rate limiting (via tmp file tracking per session) ---
SESSION_ID="${CLAUDE_SESSION_ID:-$$}"
RATE_FILE="/tmp/serendipity-rate-${SESSION_ID}"
EXISTING_COUNT=0
if [ -f "$RATE_FILE" ]; then
  EXISTING_COUNT=$(cat "$RATE_FILE" 2>/dev/null || echo "0")
fi
if [ "$EXISTING_COUNT" -ge "$MAX_PER_SESSION" ]; then
  echo "Error: Rate limit reached ($MAX_PER_SESSION findings per session). Finding NOT captured." >&2
  echo "Do NOT attempt to bypass this limit." >&2
  exit 1
fi

# --- Build scout_key for dedup ---
SCOUT_KEY="serendipity:$(echo "$TITLE" | python3 -c 'import sys,hashlib; print(hashlib.sha256(sys.stdin.read().strip().encode()).hexdigest()[:12])')"

# --- Export vars for python subprocess ---
export TITLE DESCRIPTION CATEGORY RATIONALE READINESS SCOUT_KEY RCC_URL RCC_TOKEN AGENT_NAME

# --- Post to RCC workqueue ---
RESPONSE=$(python3 -c "
import json, urllib.request, urllib.error, os, sys

payload = {
    'title': os.environ['TITLE'],
    'description': os.environ['DESCRIPTION'] + '\n\n**Category:** ' + os.environ['CATEGORY'] + '\n**Readiness:** ' + os.environ['READINESS'] + '\n**Rationale:** ' + os.environ['RATIONALE'],
    'priority': 'idea',
    'assignee': 'all',
    'source': os.environ.get('AGENT_NAME', 'unknown'),
    'tags': ['serendipity', os.environ['CATEGORY'], 'auto-captured'],
    'scout_key': os.environ['SCOUT_KEY'],
}

data = json.dumps(payload).encode()
req = urllib.request.Request(
    os.environ['RCC_URL'] + '/api/queue',
    data=data,
    headers={
        'Content-Type': 'application/json',
        'Authorization': 'Bearer ' + os.environ['RCC_TOKEN'],
    },
    method='POST'
)
try:
    with urllib.request.urlopen(req, timeout=10) as resp:
        body = json.loads(resp.read())
        print(json.dumps(body))
except urllib.error.HTTPError as e:
    body = json.loads(e.read())
    print(json.dumps({'ok': False, 'error': str(e), 'body': body}), file=sys.stderr)
    sys.exit(1)
" 2>&1) || { echo "Error: Failed to post to RCC: $RESPONSE" >&2; exit 1; }

export TITLE DESCRIPTION CATEGORY RATIONALE READINESS SCOUT_KEY RCC_URL RCC_TOKEN AGENT_NAME

# --- Update rate limit counter ---
echo $((EXISTING_COUNT + 1)) > "$RATE_FILE"

# --- Report ---
ITEM_ID=$(echo "$RESPONSE" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('item',{}).get('id','?'))" 2>/dev/null || echo "?")
IS_DUP=$(echo "$RESPONSE" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('duplicate',False))" 2>/dev/null || echo "False")

if [ "$IS_DUP" = "True" ]; then
  echo "Serendipity finding already in queue (duplicate): \"$TITLE\""
else
  echo "Serendipity finding captured: $ITEM_ID — \"$TITLE\""
fi
