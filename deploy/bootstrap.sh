#!/usr/bin/env bash
# bootstrap.sh — One-command agent bootstrap from RCC
# Installs OpenClaw, seeds workspace, configures agent identity, starts daemon.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/deploy/bootstrap.sh | \
#     bash -s -- --rcc=http://146.190.134.110:8789 --token=<bootstrap-token> --agent=boris
#
# All secrets (NVIDIA key, Mattermost token, etc.) are fetched automatically
# from RCC via the bootstrap token. No --nvidia-key or channel tokens needed.
set -euo pipefail

RCC=""
TOKEN=""
AGENT=""
# These may be overridden by CLI args, but RCC secrets take precedence if not provided
NVIDIA_KEY=""
TELEGRAM_TOKEN=""
MATTERMOST_TOKEN=""

for arg in "$@"; do
  case "$arg" in
    --rcc=*)               RCC="${arg#--rcc=}"               ;;
    --token=*)             TOKEN="${arg#--token=}"             ;;
    --agent=*)             AGENT="${arg#--agent=}"             ;;
    --nvidia-key=*)        NVIDIA_KEY="${arg#--nvidia-key=}"   ;;
    --telegram-token=*)    TELEGRAM_TOKEN="${arg#--telegram-token=}" ;;
    --mattermost-token=*)  MATTERMOST_TOKEN="${arg#--mattermost-token=}" ;;
  esac
done

if [[ -z "$RCC" || -z "$TOKEN" || -z "$AGENT" ]]; then
  echo "Usage: bootstrap.sh --rcc=<url> --token=<bootstrap-token> --agent=<name>" >&2
  exit 1
fi

# ── Colors ────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; BLUE='\033[0;34m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info()    { echo -e "${BLUE}→${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
warn()    { echo -e "${YELLOW}⚠${NC} $1"; }
error()   { echo -e "${RED}✗${NC} $1"; exit 1; }

echo ""
echo "🐻 RCC Agent Bootstrap: ${AGENT}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── 0. Clean slate ───────────────────────────────────────────────────────
info "Cleaning up previous install..."
pkill -f "openclaw.*gateway" 2>/dev/null || true
rm -rf "$HOME/.openclaw" "$HOME/.rcc" 2>/dev/null || true
success "Clean slate ready"

# ── 1. Dependency check ───────────────────────────────────────────────────
info "Checking dependencies..."
for dep in git curl; do
  command -v "$dep" &>/dev/null || error "$dep is required but not installed."
done
command -v node &>/dev/null || warn "node not found — install Node 22+ for full agent features"
success "Core dependencies present"

# ── 2. Install OpenClaw ───────────────────────────────────────────────────
if command -v openclaw &>/dev/null; then
  success "OpenClaw already installed ($(openclaw --version 2>/dev/null || echo 'version unknown'))"
else
  info "Installing OpenClaw..."
  curl -fsSL https://openclaw.ai/install.sh | bash
  # Reload PATH in case openclaw was just added
  export PATH="$HOME/.local/bin:$HOME/bin:/usr/local/bin:$PATH"
  command -v openclaw &>/dev/null || error "OpenClaw install failed — openclaw not found in PATH"
  success "OpenClaw installed"
fi

# ── 3. Clone / update RCC workspace ──────────────────────────────────────
RCC_WORKSPACE="$HOME/.rcc/workspace"
info "Setting up RCC workspace at $RCC_WORKSPACE..."
if [[ -d "$RCC_WORKSPACE/.git" ]]; then
  warn "Already cloned — pulling latest"
  git -C "$RCC_WORKSPACE" pull --ff-only
else
  git clone https://github.com/jordanhubbard/rockyandfriends.git "$RCC_WORKSPACE"
fi
success "RCC workspace ready"

# ── 4. Call bootstrap API ─────────────────────────────────────────────────
info "Consuming bootstrap token..."
BOOTSTRAP_RESP=$(curl -sf "${RCC}/api/bootstrap?token=${TOKEN}" 2>&1) || true
BOOTSTRAP_JSON=""
# Verify it looks like JSON (not an error page or empty)
if echo "$BOOTSTRAP_RESP" | grep -q '"ok":true'; then
  BOOTSTRAP_JSON="$BOOTSTRAP_RESP"
fi

if [[ -z "$BOOTSTRAP_JSON" ]]; then
  error "Bootstrap API call failed or returned invalid response.\nResponse: ${BOOTSTRAP_RESP:-<empty>}\nCheck that RCC is reachable at ${RCC} and the token is valid/unexpired."
fi

# Parse core fields using node (handles nested JSON safely)
_parse() { node -e "try{const d=JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'));process.stdout.write(String(d${1}||''))}catch(e){}" <<< "$BOOTSTRAP_JSON" 2>/dev/null || true; }

REPO_URL=$(_parse   ".repoUrl")
AGENT_TOKEN=$(_parse ".agentToken")
RCC_URL=$(_parse    ".rccUrl")
DEPLOY_KEY=$(_parse  ".deployKey")

if [[ -z "$AGENT_TOKEN" ]]; then
  error "Bootstrap response missing agentToken. Response: ${BOOTSTRAP_JSON}"
fi
success "Bootstrap token consumed — agent token issued"

# ── 4b. Extract secrets from bootstrap response ───────────────────────────
# The bootstrap API returns the full secrets bundle. Extract what we need now
# so openclaw.json can be written correctly in step 7. CLI args take precedence
# if explicitly provided (for testing/overrides); otherwise use RCC secrets.
info "Extracting secrets from bootstrap response..."
_secret() { node -e "
  try {
    const d = JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'));
    const s = d.secrets || {};
    // Try both flat key and namespaced key
    const v = s['$1'] || s['${2:-}'] || '';
    process.stdout.write(v);
  } catch(e) {}
" <<< "$BOOTSTRAP_JSON" 2>/dev/null || true; }

[[ -z "$NVIDIA_KEY"        ]] && NVIDIA_KEY=$(_secret        "NVIDIA_API_KEY"     "nvidia/api_key")
[[ -z "$MATTERMOST_TOKEN"  ]] && MATTERMOST_TOKEN=$(_secret  "MATTERMOST_TOKEN"   "mattermost/token")
[[ -z "$TELEGRAM_TOKEN"    ]] && TELEGRAM_TOKEN=$(_secret    "TELEGRAM_BOT_TOKEN" "telegram/token")

if [[ -n "$NVIDIA_KEY" ]]; then
  success "NVIDIA API key obtained from RCC secrets"
else
  warn "No NVIDIA_API_KEY in RCC secrets — will use anthropic direct (set ANTHROPIC_API_KEY in env)"
fi

# ── 5. Deploy key + SSH config ────────────────────────────────────────────
if [[ -n "$DEPLOY_KEY" ]]; then
  info "Installing deploy key..."
  mkdir -p "$HOME/.ssh"
  printf '%s\n' "$DEPLOY_KEY" > "$HOME/.ssh/rcc-deploy-key"
  chmod 600 "$HOME/.ssh/rcc-deploy-key"
  SSH_CONF="$HOME/.ssh/config"
  if ! grep -q "rcc-deploy-key" "$SSH_CONF" 2>/dev/null; then
    cat >> "$SSH_CONF" <<'SSHEOF'

Host github.com
  IdentityFile ~/.ssh/rcc-deploy-key
  StrictHostKeyChecking no
SSHEOF
    chmod 600 "$SSH_CONF"
  fi
  if [[ -n "$REPO_URL" ]]; then
    git -C "$RCC_WORKSPACE" remote set-url origin "$REPO_URL"
    git -C "$RCC_WORKSPACE" fetch origin || warn "git fetch failed — deploy key may not have read access yet"
  fi
  success "Deploy key installed"
fi

# ── 6. Seed OpenClaw workspace ────────────────────────────────────────────
OC_WORKSPACE="$HOME/.openclaw/workspace"
info "Seeding OpenClaw workspace at $OC_WORKSPACE..."
mkdir -p "$OC_WORKSPACE/memory/people" "$OC_WORKSPACE/skills"

# Copy shared files from repo
SHARED_DIR="$RCC_WORKSPACE/openclaw/shared"
if [[ -d "$SHARED_DIR" ]]; then
  cp "$SHARED_DIR/AGENTS.md" "$OC_WORKSPACE/AGENTS.md" 2>/dev/null || true
fi

# Copy agent soul if it exists; otherwise stub it
SOUL_SRC="$RCC_WORKSPACE/openclaw/souls/${AGENT}.md"
if [[ -f "$SOUL_SRC" ]]; then
  cp "$SOUL_SRC" "$OC_WORKSPACE/SOUL.md"
  success "Soul loaded from repo: openclaw/souls/${AGENT}.md"
else
  warn "No soul file found for '${AGENT}' in repo — creating stub (edit $OC_WORKSPACE/SOUL.md)"
  cat > "$OC_WORKSPACE/SOUL.md" <<SOULEOF
# SOUL.md - Who You Are

Your name is ${AGENT^}. You are a member of the Rocky & Friends agent crew.

Edit this file to define your personality, role, and voice.
Refer to rocky.md or natasha.md in openclaw/souls/ for examples.
SOULEOF
fi

# Bootstrap IDENTITY.md
cat > "$OC_WORKSPACE/IDENTITY.md" <<IDEOF
# IDENTITY.md - Who Am I?

- **Name:** ${AGENT^}
- **Agent:** ${AGENT}
- **RCC:** ${RCC_URL}
IDEOF

# Bootstrap MEMORY.md if not present
[[ -f "$OC_WORKSPACE/MEMORY.md" ]] || cat > "$OC_WORKSPACE/MEMORY.md" <<MEMEOF
# MEMORY.md - Long-Term Memory

## Identity
- My name is ${AGENT^}. I am a member of the Rocky & Friends agent crew.
- RCC hub: ${RCC_URL}
MEMEOF

# Bootstrap HEARTBEAT.md
[[ -f "$OC_WORKSPACE/HEARTBEAT.md" ]] || cat > "$OC_WORKSPACE/HEARTBEAT.md" <<HBEOF
# HEARTBEAT.md

# Standard heartbeat. Check queue and RCC health each beat.
HBEOF

success "OpenClaw workspace seeded"

# ── 7. Write openclaw.json ────────────────────────────────────────────────
OC_CONFIG="$HOME/.openclaw/openclaw.json"
info "Writing OpenClaw config..."
mkdir -p "$HOME/.openclaw"

# Build channel config fragments
TELEGRAM_FRAGMENT=""
if [[ -n "$TELEGRAM_TOKEN" ]]; then
  TELEGRAM_FRAGMENT=$(cat <<TGEOF
    "telegram": {
      "enabled": true,
      "token": "${TELEGRAM_TOKEN}"
    }
TGEOF
)
fi

MATTERMOST_FRAGMENT=""
if [[ -n "$MATTERMOST_TOKEN" ]]; then
  MATTERMOST_FRAGMENT=$(cat <<MMEOF
    "mattermost": {
      "enabled": true,
      "url": "https://chat.yourmom.photos",
      "token": "${MATTERMOST_TOKEN}"
    }
MMEOF
)
fi

# Determine model config — use NVIDIA gateway if key provided, else anthropic direct
if [[ -n "$NVIDIA_KEY" ]]; then
  MODEL_PROVIDER_JSON=$(cat <<MODEOF
      "nvidia": {
        "baseUrl": "https://inference-api.nvidia.com/v1",
        "apiKey": "${NVIDIA_KEY}",
        "api": "openai-completions",
        "models": [
          {
            "id": "azure/anthropic/claude-sonnet-4-6",
            "name": "Claude Sonnet 4.6 (NVIDIA)",
            "api": "openai-completions",
            "contextWindow": 200000,
            "maxTokens": 8192
          }
        ]
      }
MODEOF
)
  DEFAULT_MODEL="nvidia/azure/anthropic/claude-sonnet-4-6"
else
  MODEL_PROVIDER_JSON='{}'
  DEFAULT_MODEL="anthropic/claude-sonnet-4-6"
  warn "No NVIDIA_API_KEY in secrets or args — defaulting to anthropic direct. Set ANTHROPIC_API_KEY in env or add nvidia/api_key to RCC secrets."
fi

cat > "$OC_CONFIG" <<OCEOF
{
  "meta": {
    "lastTouchedVersion": "2026.3.8",
    "lastTouchedAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  },
  "gateway": {
    "mode": "local",
    "bind": "loopback",
    "port": 18789,
    "auth": {
      "mode": "none"
    }
  },
  "agents": {
    "defaults": {
      "workspace": "${OC_WORKSPACE}",
      "model": "${DEFAULT_MODEL}"
    }
  },
  "ui": {
    "assistant": {
      "name": "${AGENT^}",
      "avatar": "🤖"
    }
  },
  "models": {
    "providers": {
      ${MODEL_PROVIDER_JSON}
    }
  },
  "channels": {
    ${TELEGRAM_FRAGMENT}${TELEGRAM_FRAGMENT:+,}${MATTERMOST_FRAGMENT}
  }
}
OCEOF
success "openclaw.json written"

# ── 8. Write ~/.rcc/.env ──────────────────────────────────────────────────
info "Writing ~/.rcc/.env..."
mkdir -p "$HOME/.rcc"
ENV_FILE="$HOME/.rcc/.env"
touch "$ENV_FILE"
for key in AGENT_NAME RCC_AGENT_TOKEN RCC_URL AGENT_HOST NVIDIA_API_KEY NVIDIA_API_BASE; do
  sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
done
cat >> "$ENV_FILE" <<ENVEOF
AGENT_NAME=${AGENT}
RCC_AGENT_TOKEN=${AGENT_TOKEN}
RCC_URL=${RCC_URL}
AGENT_HOST=$(hostname)
NVIDIA_API_BASE=https://inference-api.nvidia.com/v1
NVIDIA_API_KEY=${NVIDIA_KEY}
ENVEOF
chmod 600 "$ENV_FILE"
success "~/.rcc/.env written"

# ── 8b. Write full secrets bundle to .env ────────────────────────────────
# Secrets were already fetched as part of the bootstrap response in step 4b.
# Write all flat string secrets to .env now. Never overwrites identity keys.
info "Writing secrets bundle to .env..."
node -e "
  try {
    const fs = require('fs');
    const d = JSON.parse(fs.readFileSync('/dev/stdin', 'utf8'));
    const s = d.secrets || {};
    const SKIP = new Set(['RCC_AGENT_TOKEN','RCC_URL','AGENT_NAME','AGENT_HOST']);
    const env = fs.existsSync('$ENV_FILE') ? fs.readFileSync('$ENV_FILE', 'utf8') : '';
    const lines = env.split('\n');
    const written = [];
    for (const [k, v] of Object.entries(s)) {
      // Only write flat string values (skip nested objects/alias bundles)
      if (typeof v !== 'string') continue;
      if (SKIP.has(k)) continue;
      // Remove existing line for this key
      const filtered = lines.filter(l => !l.startsWith(k + '='));
      lines.length = 0; lines.push(...filtered);
      lines.push(k + '=' + v);
      written.push(k);
    }
    fs.writeFileSync('$ENV_FILE', lines.join('\n') + '\n');
    process.stdout.write('Wrote ' + written.length + ' secrets: ' + written.slice(0,8).join(', ') + (written.length > 8 ? '...' : '') + '\n');
  } catch(e) { process.stderr.write('secrets write error: ' + e.message + '\n'); }
" <<< "$BOOTSTRAP_JSON" 2>/dev/null && success "Secrets bundle written to .env" || warn "Could not write secrets to .env (non-fatal)"

# ── 9. Start OpenClaw gateway ────────────────────────────────────────────
info "Starting OpenClaw gateway..."

# Kill any stale gateway process
pkill -f "openclaw.*gateway" 2>/dev/null || true
sleep 1

_gateway_running() {
  curl -sf http://127.0.0.1:18789/health > /dev/null 2>&1
}

# ── 9a. Try systemd user service (works on full Linux hosts) ──────────────
if systemctl --user status &>/dev/null 2>&1; then
  if command -v loginctl &>/dev/null; then
    sudo loginctl enable-linger "$(whoami)" 2>/dev/null || true
  fi
  if [[ -z "${XDG_RUNTIME_DIR:-}" ]]; then
    export XDG_RUNTIME_DIR="/run/user/$(id -u)"
    mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null || true
  fi
  openclaw gateway start --daemon 2>/dev/null && sleep 2 || true
  if _gateway_running; then
    success "OpenClaw gateway started (systemd)"
  fi
fi

# ── 9b. tmux fallback (containers / no-systemd) ───────────────────────────
if ! _gateway_running; then
  if ! command -v tmux &>/dev/null; then
    info "Installing tmux..."
    sudo apt-get install -y -q tmux 2>/dev/null || sudo yum install -y -q tmux 2>/dev/null || true
  fi
  if command -v tmux &>/dev/null; then
    tmux kill-session -t openclaw 2>/dev/null || true
    tmux new-session -d -s openclaw "openclaw gateway run --allow-unconfigured"
    sleep 3
    if _gateway_running; then
      success "OpenClaw gateway started (tmux session 'openclaw')"
    else
      warn "tmux session started but gateway not responding — check: tmux attach -t openclaw"
    fi
  fi
fi

# ── 9c. nohup last resort ─────────────────────────────────────────────────
if ! _gateway_running; then
  mkdir -p /tmp/openclaw
  nohup openclaw gateway run --allow-unconfigured > /tmp/openclaw/gateway.log 2>&1 &
  sleep 3
  if _gateway_running; then
    success "OpenClaw gateway started (nohup)"
  else
    warn "Gateway failed to start — check /tmp/openclaw/gateway.log"
  fi
fi

# ── 9d. Persist autostart in .bashrc (survives container restarts) ────────
# For containers with persistent home dirs, wire gateway autostart into .bashrc
# so it comes back on the next interactive session / container restart.
PROFILE="$HOME/.bashrc"
AUTOSTART_MARKER="# openclaw-gateway-autostart"
if ! grep -q "$AUTOSTART_MARKER" "$PROFILE" 2>/dev/null; then
  cat >> "$PROFILE" <<'AUTOSTART'
# openclaw-gateway-autostart
# Started by bootstrap.sh — restarts gateway if not already running
if command -v openclaw &>/dev/null; then
  if ! curl -sf http://127.0.0.1:18789/health >/dev/null 2>&1; then
    if command -v tmux &>/dev/null && ! tmux has-session -t openclaw 2>/dev/null; then
      tmux new-session -d -s openclaw "openclaw gateway run --allow-unconfigured" 2>/dev/null || true
    elif ! pgrep -f "openclaw.*gateway" >/dev/null 2>&1; then
      nohup openclaw gateway run --allow-unconfigured > /tmp/openclaw/gateway.log 2>&1 &
    fi
  fi
fi
AUTOSTART
  success "Gateway autostart wired into .bashrc (survives container restarts)"
fi

# ── 10. Post heartbeat ────────────────────────────────────────────────────
info "Posting heartbeat to RCC..."
curl -s -X POST "${RCC_URL}/api/heartbeat/${AGENT}" \
  -H "Authorization: Bearer ${AGENT_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{\"agent\":\"${AGENT}\",\"host\":\"$(hostname)\",\"status\":\"online\",\"version\":\"bootstrap\"}" \
  > /dev/null || warn "Heartbeat post failed (non-fatal)"

# ── 11. Done ──────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${GREEN}✅ Bootstrap complete!${NC} ${AGENT^} is alive."
echo ""
echo "  OpenClaw workspace:  ${OC_WORKSPACE}"
echo "  OpenClaw config:     ${OC_CONFIG}"
echo "  RCC workspace:       ${RCC_WORKSPACE}"
echo "  RCC env:             ${HOME}/.rcc/.env"
echo ""
if [[ -z "$TELEGRAM_TOKEN" && -z "$MATTERMOST_TOKEN" ]]; then
  echo -e "${YELLOW}  ⚠ No messaging channels configured.${NC}"
  echo "    Add TELEGRAM_BOT_TOKEN or MATTERMOST_TOKEN to RCC secrets and re-bootstrap,"
  echo "    OR edit openclaw.json and run: openclaw gateway restart"
  echo ""
fi
echo "  Next: edit ${OC_WORKSPACE}/SOUL.md to give ${AGENT^} a personality."
echo "  Then: openclaw gateway status"
echo ""
