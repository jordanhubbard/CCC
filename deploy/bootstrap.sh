#!/usr/bin/env bash
# bootstrap.sh — One-command agent bootstrap from RCC
# Usage: curl -sSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/deploy/bootstrap.sh | \
#          bash -s -- --rcc=http://146.190.134.110:8789 --token=<bootstrap-token> --agent=boris
set -euo pipefail

RCC=""
TOKEN=""
AGENT=""

for arg in "$@"; do
  case "$arg" in
    --rcc=*)   RCC="${arg#--rcc=}"   ;;
    --token=*) TOKEN="${arg#--token=}" ;;
    --agent=*) AGENT="${arg#--agent=}" ;;
  esac
done

if [[ -z "$RCC" || -z "$TOKEN" || -z "$AGENT" ]]; then
  echo "Usage: bootstrap.sh --rcc=<url> --token=<bootstrap-token> --agent=<name>" >&2
  exit 1
fi

# ── 1. Dependency check ───────────────────────────────────────────────────
echo "→ Checking dependencies..."
for dep in git curl; do
  if ! command -v "$dep" &>/dev/null; then
    echo "ERROR: $dep is required but not installed." >&2
    exit 1
  fi
done
if ! command -v node &>/dev/null; then
  echo "WARNING: node not found — some agent features may not work" >&2
fi

# ── 2. Clone / update repo ────────────────────────────────────────────────
WORKSPACE="$HOME/.rcc/workspace"
echo "→ Setting up workspace at $WORKSPACE..."
if [[ -d "$WORKSPACE/.git" ]]; then
  echo "  (already cloned, pulling latest)"
  git -C "$WORKSPACE" pull --ff-only
else
  git clone https://github.com/jordanhubbard/rockyandfriends.git "$WORKSPACE"
fi

# ── 3. Call bootstrap API ─────────────────────────────────────────────────
echo "→ Consuming bootstrap token..."
BOOTSTRAP_JSON=$(curl -sf "${RCC}/api/bootstrap?token=${TOKEN}")
if [[ -z "$BOOTSTRAP_JSON" ]]; then
  echo "ERROR: Bootstrap API returned empty response" >&2
  exit 1
fi

REPO_URL=$(echo "$BOOTSTRAP_JSON"   | grep -o '"repoUrl":"[^"]*"'   | head -1 | cut -d'"' -f4)
DEPLOY_KEY=$(echo "$BOOTSTRAP_JSON" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['deployKey'])" 2>/dev/null || \
             node -e "const d=JSON.parse(require('fs').readFileSync('/dev/stdin','utf8')); process.stdout.write(d.deployKey)" <<< "$BOOTSTRAP_JSON")
AGENT_TOKEN=$(echo "$BOOTSTRAP_JSON" | grep -o '"agentToken":"[^"]*"' | head -1 | cut -d'"' -f4)
RCC_URL=$(echo "$BOOTSTRAP_JSON"     | grep -o '"rccUrl":"[^"]*"'     | head -1 | cut -d'"' -f4)

if [[ -z "$AGENT_TOKEN" ]]; then
  echo "ERROR: Failed to parse agentToken from bootstrap response" >&2
  echo "Response: $BOOTSTRAP_JSON" >&2
  exit 1
fi

# ── 4. Write deploy key ───────────────────────────────────────────────────
echo "→ Installing deploy key..."
mkdir -p "$HOME/.ssh"
printf '%s\n' "$DEPLOY_KEY" > "$HOME/.ssh/rcc-deploy-key"
chmod 600 "$HOME/.ssh/rcc-deploy-key"

# ── 5. SSH config ─────────────────────────────────────────────────────────
SSH_CONF="$HOME/.ssh/config"
if ! grep -q "IdentityFile ~/.ssh/rcc-deploy-key" "$SSH_CONF" 2>/dev/null; then
  echo "→ Adding SSH config for github.com..."
  cat >> "$SSH_CONF" <<'EOF'

Host github.com
  IdentityFile ~/.ssh/rcc-deploy-key
  StrictHostKeyChecking no
EOF
  chmod 600 "$SSH_CONF"
fi

# ── 6. Rewrite git remote ─────────────────────────────────────────────────
if [[ -n "$REPO_URL" ]]; then
  echo "→ Setting git remote to $REPO_URL..."
  git -C "$WORKSPACE" remote set-url origin "$REPO_URL"
fi

# ── 7. Verify fetch ───────────────────────────────────────────────────────
echo "→ Verifying git fetch..."
if ! git -C "$WORKSPACE" fetch origin; then
  echo "ERROR: git fetch failed — check deploy key and repo URL" >&2
  exit 1
fi

# ── 8. Write ~/.rcc/.env ──────────────────────────────────────────────────
echo "→ Writing ~/.rcc/.env..."
mkdir -p "$HOME/.rcc"
ENV_FILE="$HOME/.rcc/.env"

# Preserve existing values, only overwrite the keys we manage
touch "$ENV_FILE"
for key in AGENT_NAME RCC_AGENT_TOKEN RCC_URL AGENT_HOST; do
  sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
done
cat >> "$ENV_FILE" <<EOF
AGENT_NAME=${AGENT}
RCC_AGENT_TOKEN=${AGENT_TOKEN}
RCC_URL=${RCC_URL}
AGENT_HOST=$(hostname)
EOF

# ── 9. Post heartbeat ─────────────────────────────────────────────────────
echo "→ Posting heartbeat..."
curl -s -X POST "${RCC_URL}/api/heartbeat/${AGENT}" \
  -H "Authorization: Bearer ${AGENT_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{\"agent\":\"${AGENT}\",\"host\":\"$(hostname)\",\"status\":\"online\"}" \
  > /dev/null || echo "WARNING: heartbeat post failed (non-fatal)"

# ── 10. Done ──────────────────────────────────────────────────────────────
echo ""
echo "✅ Bootstrap complete. ${AGENT} is online at ${RCC_URL}"
