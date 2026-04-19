#!/usr/bin/env bash
# fix-hermes-config.sh — Rewrite ~/.hermes/config.yaml from current ~/.acc/.env secrets
#                        and restart hermes-gateway.  Safe to run on an existing agent
#                        without a full re-onboard.
#
# Usage (local):   bash ~/.acc/workspace/deploy/fix-hermes-config.sh
# Usage (remote):  acc.exec command=fix_hermes_config  (via commands.json entry)
set -euo pipefail

ACC_DIR="${HOME}/.acc"
ENV_FILE="${ACC_DIR}/.env"
HERMES_CONFIG="${HOME}/.hermes/config.yaml"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
ok()   { echo -e "${GREEN}✓${NC} $1"; }
warn() { echo -e "${YELLOW}⚠${NC} $1"; }
die()  { echo -e "${RED}✗${NC} $1" >&2; exit 1; }

# ── 1. Load .env ──────────────────────────────────────────────────────────────
[[ -f "$ENV_FILE" ]] || die "~/.acc/.env not found — run bootstrap first"

# shellcheck disable=SC1090
set -a; source "$ENV_FILE"; set +a

[[ -n "${ACC_URL:-}"         ]] || die "ACC_URL missing from .env"
[[ -n "${ACC_AGENT_TOKEN:-}" ]] || die "ACC_AGENT_TOKEN missing from .env"
[[ -n "${AGENT_NAME:-}"      ]] || die "AGENT_NAME missing from .env"

ok "Loaded .env (agent=${AGENT_NAME}, hub=${ACC_URL})"

# ── 2. Re-fetch Slack tokens from ACC API ────────────────────────────────────
_json_get() {
    local json="$1"; shift
    for path in "$@"; do
        val=$(echo "$json" | python3 -c "
import sys, json
try:
    d = json.load(sys.stdin)
    parts = '${path}'.lstrip('.').split('.')
    for p in parts:
        d = d[p]
    print(d)
except Exception:
    pass
" 2>/dev/null) && [[ -n "$val" ]] && echo "$val" && return
    done
}

SLACK_BOT_TOKEN="${SLACK_BOT_TOKEN:-}"
SLACK_APP_TOKEN="${SLACK_APP_TOKEN:-}"

SLACK_BUNDLE=$(curl -sf "${ACC_URL}/api/secrets/${AGENT_NAME}_slack" \
    -H "Authorization: Bearer ${ACC_AGENT_TOKEN}" 2>/dev/null || echo "")

if [[ -n "$SLACK_BUNDLE" ]]; then
    fresh_bot=$(_json_get "$SLACK_BUNDLE" .secrets.SLACK_BOT_TOKEN || true)
    fresh_app=$(_json_get "$SLACK_BUNDLE" .secrets.SLACK_APP_TOKEN || true)
    [[ -n "$fresh_bot" ]] && SLACK_BOT_TOKEN="$fresh_bot"
    [[ -n "$fresh_app" ]] && SLACK_APP_TOKEN="$fresh_app"
    ok "Slack tokens refreshed from ACC API"
else
    warn "Could not fetch Slack tokens from ACC API — using .env values (may be stale)"
fi

# ── 3. Write ~/.hermes/config.yaml ───────────────────────────────────────────
mkdir -p "$(dirname "$HERMES_CONFIG")"

# Back up existing config
if [[ -f "$HERMES_CONFIG" ]]; then
    cp "$HERMES_CONFIG" "${HERMES_CONFIG}.bak"
    warn "Backed up existing config.yaml → config.yaml.bak"
fi

cat > "$HERMES_CONFIG" <<HCEOF
env:
  ACC_URL: "${ACC_URL}"
  ACC_AGENT_TOKEN: "${ACC_AGENT_TOKEN}"
  AGENT_NAME: "${AGENT_NAME}"
HCEOF

[[ -n "$SLACK_BOT_TOKEN" ]] && echo "  SLACK_BOT_TOKEN: \"${SLACK_BOT_TOKEN}\"" >> "$HERMES_CONFIG"
[[ -n "$SLACK_APP_TOKEN" ]] && echo "  SLACK_APP_TOKEN: \"${SLACK_APP_TOKEN}\"" >> "$HERMES_CONFIG"

NVIDIA_KEY="${NVIDIA_API_KEY:-}"
if [[ -n "$NVIDIA_KEY" ]]; then
    cat >> "$HERMES_CONFIG" <<HPEOF

model:
  default: azure/anthropic/claude-sonnet-4-6
  provider: nvidia
  base_url: https://inference-api.nvidia.com/v1/
  api_key: "${NVIDIA_KEY}"

providers:
  nvidia:
    api: https://inference-api.nvidia.com/v1/
    name: nvidia
    api_key: "${NVIDIA_KEY}"
    transport: chat_completions

fallback_providers: []

toolsets:
- hermes-cli

agent:
  max_turns: 90
  gateway_timeout: 0
  restart_drain_timeout: 60
HPEOF
    ok "hermes providers.nvidia written with API key"
else
    warn "NVIDIA_API_KEY not set — skipping model/providers block"
fi

chmod 600 "$HERMES_CONFIG"
ok "~/.hermes/config.yaml written"

# ── 4. Restart hermes-gateway ─────────────────────────────────────────────────
_restarted=false

# macOS: launchd
if command -v launchctl &>/dev/null; then
    for _plist in \
        "${HOME}/Library/LaunchAgents/com.acc.hermes-worker.plist" \
        "${HOME}/Library/LaunchAgents/com.hermes.gateway.plist"; do
        if [[ -f "$_plist" ]]; then
            launchctl unload "$_plist" 2>/dev/null || true
            launchctl load   "$_plist" 2>/dev/null && _restarted=true && ok "Reloaded $(basename "$_plist")" || true
        fi
    done
fi

# Linux: systemd
if command -v systemctl &>/dev/null && ! $_restarted; then
    for _svc in acc-hermes-worker hermes-gateway; do
        if systemctl is-enabled --quiet "$_svc" 2>/dev/null; then
            systemctl restart "$_svc" && _restarted=true && ok "Restarted ${_svc}.service" && break
        fi
    done
fi

# supervisord fallback
if ! $_restarted && command -v supervisorctl &>/dev/null; then
    if supervisorctl status hermes-gateway &>/dev/null; then
        supervisorctl restart hermes-gateway && _restarted=true && ok "Restarted hermes-gateway via supervisord"
    fi
fi

$_restarted || warn "hermes-gateway not found in launchd/systemd/supervisord — restart it manually"

echo ""
ok "fix-hermes-config complete"
