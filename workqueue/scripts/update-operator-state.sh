#!/usr/bin/env bash
# update-operator-state.sh — Read or patch operator-state.json on MinIO
# Usage:
#   update-operator-state.sh read                  # Print current state
#   update-operator-state.sh patch <jq-filter>     # Apply a jq patch and re-upload
#   update-operator-state.sh append-context <json> # Append a context entry to recent_context[]
#   update-operator-state.sh set-last-seen <channel> <note> # Update last_seen
#
# Examples:
#   update-operator-state.sh read
#   update-operator-state.sh patch '.current_location = "Taipei, Taiwan"'
#   update-operator-state.sh append-context '{"ts":"2026-03-20T00:00:00Z","summary":"operator arrived in destination"}'
#   update-operator-state.sh set-last-seen "slack-your-workspace" "Discussed render pipeline"

set -e

MC=${MC_BIN:-mc}
ALIAS="${MINIO_ALIAS:-local}"
BUCKET="agents/shared/operator-state.json"
TMPFILE=$(mktemp /tmp/operator-state-XXXXXX.json)
AGENT="${JKH_STATE_AGENT:-rocky}"
NOW=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

cleanup() { rm -f "$TMPFILE"; }
trap cleanup EXIT

# Download current state
$MC cat "$ALIAS/$BUCKET" > "$TMPFILE" 2>/dev/null || echo '{}' > "$TMPFILE"

CMD="${1:-read}"

case "$CMD" in
  read)
    cat "$TMPFILE"
    ;;

  patch)
    FILTER="$2"
    if [ -z "$FILTER" ]; then
      echo "Usage: $0 patch '<jq-filter>'" >&2
      exit 1
    fi
    UPDATED=$(jq --arg agent "$AGENT" --arg ts "$NOW" \
      "$FILTER | .last_updated_by = \$agent | .last_updated_ts = \$ts" "$TMPFILE")
    echo "$UPDATED" > "$TMPFILE"
    $MC cp "$TMPFILE" "$ALIAS/$BUCKET"
    echo "✅ jkh-state.json updated on MinIO"
    ;;

  append-context)
    ENTRY="$2"
    if [ -z "$ENTRY" ]; then
      echo "Usage: $0 append-context '<json-object>'" >&2
      exit 1
    fi
    UPDATED=$(jq --argjson entry "$ENTRY" --arg agent "$AGENT" --arg ts "$NOW" \
      '.recent_context += [$entry] | .recent_context = (.recent_context | sort_by(.ts) | reverse | .[0:20]) | .last_updated_by = $agent | .last_updated_ts = $ts' \
      "$TMPFILE")
    echo "$UPDATED" > "$TMPFILE"
    $MC cp "$TMPFILE" "$ALIAS/$BUCKET"
    echo "✅ Context appended to jkh-state.json on MinIO"
    ;;

  set-last-seen)
    CHANNEL="$2"
    NOTE="$3"
    UPDATED=$(jq --arg channel "$CHANNEL" --arg note "$NOTE" --arg ts "$NOW" --arg agent "$AGENT" \
      '.last_seen = {"channel": $channel, "ts": $ts, "note": $note} | .last_updated_by = $agent | .last_updated_ts = $ts' \
      "$TMPFILE")
    echo "$UPDATED" > "$TMPFILE"
    $MC cp "$TMPFILE" "$ALIAS/$BUCKET"
    echo "✅ last_seen updated in jkh-state.json on MinIO"
    ;;

  *)
    echo "Unknown command: $CMD" >&2
    echo "Commands: read | patch | append-context | set-last-seen" >&2
    exit 1
    ;;
esac
