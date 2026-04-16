#!/usr/bin/env bash
# fleet-sync.sh — Push current workspace state to all agents via ClawBus.
#
# Run this after committing and pushing changes to GitHub:
#
#   git push && bash deploy/fleet-sync.sh --ccc=http://<hub>:8788 --token=<token>
#
# If ~/.ccc/.env exists with CCC_URL and CCC_AGENT_TOKEN set, flags are optional.
#
# What it does:
#   1. Mirrors ~/.ccc/workspace → MinIO (agentfs) so it's available via mc
#   2. Broadcasts rcc.update to ClawBus so all agents run agent-pull.sh NOW
#      instead of waiting up to 10 minutes for the scheduled timer
#   3. Reports online agents from /bus/presence
#
# Flags:
#   --ccc=URL       Hub URL (overrides CCC_URL from .env)
#   --token=TOKEN   Agent token (overrides CCC_AGENT_TOKEN from .env)
#   --dry-run       Show what would happen without doing it
#   --skip-mirror   Skip the MinIO mirror step (just send the bus message)
#   --branch=NAME   Specify branch (default: current git branch)
#   --component=X   Component name in rcc.update body (default: workspace)

set -euo pipefail

# ── Parse args ─────────────────────────────────────────────────────────────────
DRY_RUN=false
SKIP_MIRROR=false
BRANCH=""
COMPONENT="workspace"
CCC_URL_ARG=""
CCC_TOKEN_ARG=""

for arg in "$@"; do
  case "$arg" in
    --ccc=*)          CCC_URL_ARG="${arg#--ccc=}" ;;
    --token=*)        CCC_TOKEN_ARG="${arg#--token=}" ;;
    --dry-run)        DRY_RUN=true ;;
    --skip-mirror)    SKIP_MIRROR=true ;;
    --branch=*)       BRANCH="${arg#--branch=}" ;;
    --component=*)    COMPONENT="${arg#--component=}" ;;
    -h|--help)
      echo "Usage: fleet-sync.sh [--ccc=URL] [--token=TOKEN] [--dry-run] [--skip-mirror] [--branch=NAME] [--component=NAME]"
      exit 0 ;;
  esac
done

# ── Load env ───────────────────────────────────────────────────────────────────
CCC_DIR="${HOME}/.ccc"
ENV_FILE="${CCC_DIR}/.env"
WORKSPACE="${CCC_DIR}/workspace"

# Also try loading from the workspace's own .env-style files
for _env in "$ENV_FILE" ".env" "${WORKSPACE}/.env"; do
  [[ -f "$_env" ]] && { set -a; source "$_env"; set +a; } || true
done

CCC_URL="${CCC_URL:-}"
CCC_AGENT_TOKEN="${CCC_AGENT_TOKEN:-}"
AGENT_NAME="${AGENT_NAME:-jkh}"
MINIO_ALIAS="${MINIO_ALIAS:-ccc-hub}"
# ClawFS S3 gateway (port 9100) — externally accessible from any agent or operator machine.
# MinIO (port 9000) is localhost-only on the hub.
CLAWFS_BUCKET="${CLAWFS_BUCKET:-clawfs}"
CLAWFS_REPO_PATH="${CLAWFS_REPO_PATH:-repos/CCC}"

# CLI flags override .env
[[ -n "$CCC_URL_ARG"   ]] && CCC_URL="$CCC_URL_ARG"
[[ -n "$CCC_TOKEN_ARG" ]] && CCC_AGENT_TOKEN="$CCC_TOKEN_ARG"

if [[ -z "$CCC_URL" ]]; then
  echo "ERROR: CCC_URL not set. Pass --ccc=<url> or add CCC_URL to ~/.ccc/.env" >&2
  exit 1
fi

CCC_URL="${CCC_URL%/}"

# Resolve git repo — prefer the directory containing this script, then WORKSPACE
SCRIPT_REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd || echo "")"
for _candidate in "$SCRIPT_REPO" "$WORKSPACE" "$(pwd)"; do
  if [[ -d "${_candidate}/.git" ]]; then
    GIT_REPO="$_candidate"
    break
  fi
done
GIT_REPO="${GIT_REPO:-$WORKSPACE}"

# Resolve branch
if [[ -z "$BRANCH" ]]; then
  BRANCH=$(git -C "${GIT_REPO}" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "main")
fi

REV=$(git -C "${GIT_REPO}" rev-parse --short HEAD 2>/dev/null || echo "unknown")

# ── Colors ─────────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; BLUE='\033[0;34m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info()    { echo -e "${BLUE}→${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
warn()    { echo -e "${YELLOW}⚠${NC} $1"; }
dry()     { echo -e "${YELLOW}[DRY RUN]${NC} $1"; }

echo ""
echo "🔄 CCC Fleet Sync  (rev=${REV} branch=${BRANCH})"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── Step 1: MinIO mirror ───────────────────────────────────────────────────────
if [[ "$SKIP_MIRROR" == false ]]; then
  info "Mirroring workspace → ClawFS (agentfs)..."
  MIRROR_SRC="${GIT_REPO}/"
  # ClawFS S3 gateway (port 9100) — externally accessible. Path mirrors the on-disk layout
  # at /home/jkh/clawfs/repos/CCC on the hub. No --remove: ClawFS may contain built
  # artifacts and compiled binaries not tracked in git.
  MIRROR_DST="${MINIO_ALIAS}/${CLAWFS_BUCKET}/${CLAWFS_REPO_PATH}/"

  if command -v mc &>/dev/null; then
    # Auto-configure alias from hub secrets if not yet set
    if ! mc ls "${MINIO_ALIAS}" > /dev/null 2>&1; then
      info "mc alias '${MINIO_ALIAS}' not set — fetching credentials from hub..."
      _hub_base="${CCC_URL%:*}"  # strip port, keep scheme+host
      _clawfs_url="${_hub_base}:9100"
      _ak=$(curl -sf -H "Authorization: Bearer ${CCC_AGENT_TOKEN}" \
        "${CCC_URL}/api/secrets/agentfs%2Faccess_key" 2>/dev/null | \
        python3 -c "import json,sys; print(json.load(sys.stdin).get('value',''))" 2>/dev/null || echo "")
      _sk=$(curl -sf -H "Authorization: Bearer ${CCC_AGENT_TOKEN}" \
        "${CCC_URL}/api/secrets/agentfs%2Fsecret_key" 2>/dev/null | \
        python3 -c "import json,sys; print(json.load(sys.stdin).get('value',''))" 2>/dev/null || echo "")
      if [[ -n "$_ak" && -n "$_sk" ]]; then
        mc alias set "${MINIO_ALIAS}" "${_clawfs_url}" "${_ak}" "${_sk}" > /dev/null 2>&1
        success "mc alias '${MINIO_ALIAS}' configured (${_clawfs_url})"
        # Persist to .env for next run
        _env_file="${CCC_DIR}/.env"
        if [[ -f "$_env_file" ]]; then
          sed -i.bak "/^MINIO_ALIAS=/d; /^CLAWFS_URL=/d" "$_env_file" 2>/dev/null || true
          printf 'MINIO_ALIAS=%s\nCLAWFS_URL=%s\n' "${MINIO_ALIAS}" "${_clawfs_url}" >> "$_env_file"
          rm -f "${_env_file}.bak"
        fi
      else
        warn "Could not fetch agentfs credentials from hub — skipping mirror"
        warn "  Set mc alias manually: mc alias set ${MINIO_ALIAS} http://<hub>:9100 <ak> <sk>"
      fi
    fi

    if mc ls "${MINIO_ALIAS}" > /dev/null 2>&1; then
      if [[ "$DRY_RUN" == true ]]; then
        dry "Would run: mc mirror --overwrite \"${MIRROR_SRC}\" \"${MIRROR_DST}\""
      else
        mc mirror --overwrite \
          --exclude ".git/*" \
          --exclude "node_modules/*" \
          --exclude "*.log" \
          "${MIRROR_SRC}" "${MIRROR_DST}" 2>&1 | tail -5
        success "Workspace mirrored to ClawFS: ${MIRROR_DST}"
      fi
    fi
  else
    warn "mc not found — skipping mirror (run: make deps)"
  fi
else
  info "ClawFS mirror skipped (--skip-mirror)"
fi

# ── Step 2: Show online agents from ClawBus presence ─────────────────────────
info "Checking agent presence..."
PRESENCE_JSON=$(curl -sf --max-time 10 \
  -H "Authorization: Bearer ${CCC_AGENT_TOKEN}" \
  "${CCC_URL}/bus/presence" 2>/dev/null || echo "")
if [[ -n "$PRESENCE_JSON" ]]; then
  ONLINE_AGENTS=$(python3 -c "
import json, sys
try:
    data = json.loads(sys.argv[1])
    agents = list(data.keys()) if isinstance(data, dict) else []
    print(', '.join(agents) if agents else '(none)')
except Exception:
    print('(could not parse)')
" "$PRESENCE_JSON" 2>/dev/null || echo "(unknown)")
  info "Online agents: ${ONLINE_AGENTS}"
else
  warn "Could not reach /bus/presence — hub may be down or unreachable"
fi

# ── Step 3: Broadcast rcc.update ──────────────────────────────────────────────
info "Broadcasting rcc.update to all agents..."

UPDATE_BODY=$(python3 -c "
import json
print(json.dumps({'component': '${COMPONENT}', 'branch': '${BRANCH}', 'rev': '${REV}'}))
" 2>/dev/null || echo "{\"component\":\"${COMPONENT}\",\"branch\":\"${BRANCH}\"}")

MSG_JSON=$(python3 -c "
import json
print(json.dumps({
  'from': '${AGENT_NAME}',
  'to': 'all',
  'type': 'rcc.update',
  'subject': 'workspace sync ${REV}',
  'body': '${UPDATE_BODY}'
}))
" 2>/dev/null)

if [[ "$DRY_RUN" == true ]]; then
  dry "Would POST to ${CCC_URL}/bus/send:"
  dry "  $MSG_JSON"
else
  if [[ -z "$CCC_AGENT_TOKEN" ]]; then
    warn "CCC_AGENT_TOKEN not set — bus message will fail if hub requires auth"
  fi
  RESP=$(curl -sf --max-time 15 \
    -X POST "${CCC_URL}/bus/send" \
    -H "Authorization: Bearer ${CCC_AGENT_TOKEN}" \
    -H "Content-Type: application/json" \
    -d "$MSG_JSON" 2>&1) || RESP=""

  if echo "$RESP" | grep -q '"ok":true'; then
    SEQ=$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('message',{}).get('seq','?'))" "$RESP" 2>/dev/null || echo "?")
    success "rcc.update broadcast sent (seq=${SEQ})"
  else
    warn "Bus send may have failed. Response: ${RESP:-<empty>}"
    warn "  Agents will still sync on their next 10-minute timer cycle."
  fi
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${GREEN}✓ Fleet sync complete${NC}"
echo ""
echo "  Agents subscribed to ClawBus will pull immediately."
echo "  Others will pick it up within 10 minutes via ccc-agent-pull timer."
echo ""
echo "  Workspace available via ClawFS at:"
echo "    mc ls ${MINIO_ALIAS}/${CLAWFS_BUCKET}/${CLAWFS_REPO_PATH}/"
echo ""
