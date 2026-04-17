#!/usr/bin/env bash
# migrate.sh — Migrate an existing agent workspace to current CCC baseline
# Usage: bash migrate.sh --agent=bullwinkle --ccc=http://<CCC_HOST>:8789 --token=<static-agent-token>
# Run ON the agent machine (not remotely)
set -euo pipefail

# ── Parse args ──────────────────────────────────────────────────────────────
AGENT=""
CCC=""
TOKEN=""

for arg in "$@"; do
  case $arg in
    --agent=*) AGENT="${arg#*=}" ;;
    --ccc=*)   CCC="${arg#*=}" ;;
    --token=*) TOKEN="${arg#*=}" ;;
    *) echo "Unknown argument: $arg" >&2; exit 1 ;;
  esac
done

if [[ -z "$AGENT" ]]; then
  echo "Error: --agent is required" >&2
  exit 1
fi
if [[ -z "$CCC" ]]; then
  echo "Error: --ccc is required (e.g. --ccc=http://<CCC_HOST>:8789)" >&2
  exit 1
fi
if [[ -z "$TOKEN" ]]; then
  echo "Error: --token is required" >&2
  exit 1
fi

if [[ -d "${HOME}/.ccc/workspace" ]]; then
  WORKSPACE="${HOME}/.ccc/workspace"
  ENV_FILE="${HOME}/.ccc/.env"
else
  echo "Error: no workspace found at ~/.ccc/workspace" >&2
  exit 1
fi
mkdir -p "$(dirname "$ENV_FILE")"

echo "=== migrate.sh — agent: $AGENT ==="
echo "CCC: $CCC"
echo "Workspace: $WORKSPACE"
echo ""

# ── Step 1: Verify CCC repo ─────────────────────────────────────
echo "[1/7] Verifying workspace is a CCC repo..."
cd "$WORKSPACE"
REMOTE_URL=$(git remote get-url origin 2>/dev/null || echo "")
if [[ "$REMOTE_URL" != *"CCC"* ]]; then
  echo "Error: workspace remote is not a CCC repo: $REMOTE_URL" >&2
  exit 1
fi
echo "      Remote: $REMOTE_URL — OK"

# ── Step 2: Fast-forward to latest main ─────────────────────────────────────
echo "[2/7] Fetching and merging origin/main (fast-forward only)..."
git fetch origin
if ! git merge origin/main --ff-only; then
  echo "Error: fast-forward merge failed — resolve conflicts manually and re-run." >&2
  exit 1
fi
echo "      Merge OK"

# ── Step 3: Verify DIRECTIVES.md exists ─────────────────────────────────────
echo "[3/7] Verifying DIRECTIVES.md exists..."
if [[ ! -f "$WORKSPACE/DIRECTIVES.md" ]]; then
  echo "Error: DIRECTIVES.md not found after pull — something went wrong." >&2
  exit 1
fi
echo "      DIRECTIVES.md found — OK"

# ── Step 4: Update .env ──────────────────────────────────────────────────────
echo "[4/7] Updating .env..."
touch "$ENV_FILE"

# macOS vs Linux sed -i compatibility
if [[ "$(uname)" == "Darwin" ]]; then
  SED_I() { sed -i '' "$@"; }
else
  SED_I() { sed -i "$@"; }
fi

# Helper: set or add a key in the env file (preserves all other keys)
set_env_key() {
  local key="$1"
  local value="$2"
  if grep -q "^${key}=" "$ENV_FILE" 2>/dev/null; then
    SED_I "s|^${key}=.*|${key}=${value}|" "$ENV_FILE"
  else
    echo "${key}=${value}" >> "$ENV_FILE"
  fi
}

set_env_key "CCC_AGENT_TOKEN" "$TOKEN"
set_env_key "CCC_URL" "$CCC"
set_env_key "AGENT_NAME" "$AGENT"
set_env_key "AGENT_HOST" "$(hostname)"

echo "      .env updated — CCC_AGENT_TOKEN, CCC_URL, AGENT_NAME, AGENT_HOST set"

# ── Step 4b: Fetch secrets from CCC ─────────────────────────────────────────
echo "[4b/7] Fetching secrets from CCC..."
_CCC_AGENT="${CCC_AGENT:-$HOME/.ccc/bin/ccc-agent}"
[ ! -x "$_CCC_AGENT" ] && _CCC_AGENT="$(command -v ccc-agent 2>/dev/null || echo "")"
_secrets_synced=0
if [ -x "$_CCC_AGENT" ]; then
  for _alias in slack mattermost minio qdrant nvidia github; do
    _resp=$(curl -sf -H "Authorization: Bearer ${TOKEN}" \
      "${CCC}/api/secrets/${_alias}" 2>/dev/null || true)
    [[ -z "$_resp" ]] && continue
    if echo "$_resp" | grep -q '"secrets"'; then
      while IFS='=' read -r _k _v; do
        [[ -z "$_k" ]] && continue
        case "$_k" in CCC_AGENT_TOKEN|CCC_URL|AGENT_NAME|AGENT_HOST) continue ;; esac
        set_env_key "$_k" "$_v"
        _secrets_synced=$((_secrets_synced + 1))
      done < <(echo "$_resp" | "$_CCC_AGENT" json pairs .secrets 2>/dev/null)
    fi
  done
  echo "      Secrets synced: ${_secrets_synced} env vars fetched from CCC"
else
  echo "      ccc-agent not available — skipping CCC secrets fetch (run migration 0011 first)"
fi

# ── Step 5: Post heartbeat ───────────────────────────────────────────────────
echo "[5/7] Posting heartbeat to CCC..."
HEARTBEAT_RESPONSE=$(curl -s -w "\n%{http_code}" -X POST \
  "${CCC}/api/heartbeat/${AGENT}" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{\"source\":\"migrate.sh\",\"status\":\"online\"}")
HEARTBEAT_HTTP=$(echo "$HEARTBEAT_RESPONSE" | tail -1)
HEARTBEAT_BODY=$(echo "$HEARTBEAT_RESPONSE" | head -1)
echo "      HTTP $HEARTBEAT_HTTP: $HEARTBEAT_BODY"
if [[ "$HEARTBEAT_HTTP" != 2* ]]; then
  echo "Warning: heartbeat returned non-2xx status ($HEARTBEAT_HTTP)" >&2
fi

# ── Step 6: Verify agent appears in status ───────────────────────────────────
echo "[6/7] Verifying agent status at CCC..."
STATUS_RESPONSE=$(curl -s "${CCC}/api/agents/status" \
  -H "Authorization: Bearer ${TOKEN}")
if echo "$STATUS_RESPONSE" | grep -q "\"$AGENT\""; then
  echo "      Agent '$AGENT' found in status response — OK"
else
  echo "Warning: agent '$AGENT' not found in status response." >&2
  echo "      Response: $STATUS_RESPONSE"
fi

# ── Step 7: Ingest memory (non-fatal) ────────────────────────────────────────
echo "[7/7] Checking for node / Qdrant ingest..."
INGEST_RESULT="skipped (node not available)"
if command -v node &>/dev/null; then
  INGEST_SCRIPT="$WORKSPACE.ccc/scripts/ingest-memory.mjs"
  if [[ -f "$INGEST_SCRIPT" ]]; then
    echo "      Running ingest-memory.mjs..."
    if node "$INGEST_SCRIPT" 2>&1; then
      INGEST_RESULT="success"
    else
      INGEST_RESULT="failed (non-fatal)"
      echo "Warning: ingest-memory.mjs failed — continuing." >&2
    fi
  else
    INGEST_RESULT="skipped (ingest-memory.mjs not found)"
  fi
fi
echo "      Ingest: $INGEST_RESULT"

# ── Summary ──────────────────────────────────────────────────────────────────
echo ""
echo "=== Migration complete ==="
echo "  Agent:     $AGENT"
echo "  Workspace: $WORKSPACE (on branch $(git -C "$WORKSPACE" rev-parse --abbrev-ref HEAD), $(git -C "$WORKSPACE" rev-parse --short HEAD))"
echo "  .env:      updated (CCC_AGENT_TOKEN, CCC_URL, AGENT_NAME, AGENT_HOST + CCC secrets)"
echo "  Heartbeat: HTTP $HEARTBEAT_HTTP"
echo "  Ingest:    $INGEST_RESULT"
echo ""
echo "Run 'systemctl --user restart ccc-agent' (or equivalent) if the agent service is running."
