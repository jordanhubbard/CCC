#!/usr/bin/env bash
# bootstrap.sh — One-command agent bootstrap from RCC
# Installs OpenClaw, seeds workspace, configures agent identity, starts daemon.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/deploy/bootstrap.sh | \
#     bash -s -- --rcc=http://146.190.134.110:8789 --token=<bootstrap-token> --agent=boris \
#                --nvidia-key=<key> [--telegram-token=<tok>] [--mattermost-token=<tok>]
set -euo pipefail

RCC=""
TOKEN=""
AGENT=""
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
  echo "Usage: bootstrap.sh --rcc=<url> --token=<bootstrap-token> --agent=<name> [--nvidia-key=<key>] [--telegram-token=<tok>] [--mattermost-token=<tok>]" >&2
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
BOOTSTRAP_JSON=$(curl -sf "${RCC}/api/bootstrap?token=${TOKEN}" || true)
if [[ -z "$BOOTSTRAP_JSON" ]]; then
  warn "Bootstrap API returned empty — proceeding without RCC provisioning (offline mode)"
  AGENT_TOKEN="offline"
  RCC_URL="$RCC"
  REPO_URL=""
  DEPLOY_KEY=""
else
  REPO_URL=$(echo    "$BOOTSTRAP_JSON" | grep -o '"repoUrl":"[^"]*"'   | head -1 | cut -d'"' -f4 || true)
  AGENT_TOKEN=$(echo "$BOOTSTRAP_JSON" | grep -o '"agentToken":"[^"]*"' | head -1 | cut -d'"' -f4 || true)
  RCC_URL=$(echo     "$BOOTSTRAP_JSON" | grep -o '"rccUrl":"[^"]*"'     | head -1 | cut -d'"' -f4 || true)
  DEPLOY_KEY=$(node -e "try{const d=JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'));process.stdout.write(d.deployKey||'')}catch(e){}" <<< "$BOOTSTRAP_JSON" 2>/dev/null || true)

  if [[ -z "$AGENT_TOKEN" ]]; then
    warn "No agentToken in bootstrap response — continuing without RCC token"
    AGENT_TOKEN="offline"
    RCC_URL="$RCC"
  else
    success "Bootstrap token consumed"
  fi
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
  warn "No --nvidia-key provided — defaulting to anthropic direct. Set ANTHROPIC_API_KEY in env."
fi

cat > "$OC_CONFIG" <<OCEOF
{
  "meta": {
    "lastTouchedVersion": "2026.3.8",
    "lastTouchedAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
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

# ── 9. Start OpenClaw gateway ────────────────────────────────────────────
info "Starting OpenClaw gateway..."

# Fix systemd user session for headless/container environments
# This is required on HORDE and similar setups where systemd user bus isn't available
if command -v loginctl &>/dev/null; then
  sudo loginctl enable-linger "$(whoami)" 2>/dev/null || true
fi

# Ensure XDG_RUNTIME_DIR is set (required for systemd user services)
if [[ -z "${XDG_RUNTIME_DIR:-}" ]]; then
  export XDG_RUNTIME_DIR="/run/user/$(id -u)"
  mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null || true
fi

# Persist XDG_RUNTIME_DIR in shell profile
PROFILE="$HOME/.bashrc"
if ! grep -q "XDG_RUNTIME_DIR" "$PROFILE" 2>/dev/null; then
  echo 'export XDG_RUNTIME_DIR=/run/user/$(id -u)' >> "$PROFILE"
fi

# Kill any existing gateway process
pkill -f "openclaw.*gateway" 2>/dev/null || true
sleep 1

# Try systemd user service first; fall back to tmux background session
if systemctl --user status &>/dev/null 2>&1; then
  openclaw gateway start --daemon 2>/dev/null && \
    success "OpenClaw gateway started (systemd)" || \
    warn "systemd start failed — falling back to tmux"
fi

# tmux fallback (reliable on HORDE / containers / SSH-only boxes)
if ! openclaw gateway status --quiet 2>/dev/null; then
  if command -v tmux &>/dev/null; then
    tmux kill-session -t openclaw 2>/dev/null || true
    tmux new-session -d -s openclaw "openclaw gateway start"
    sleep 2
    if openclaw gateway status --quiet 2>/dev/null; then
      success "OpenClaw gateway started (tmux session 'openclaw')"
    else
      warn "Gateway may not be running — check: tmux attach -t openclaw"
    fi
  else
    # Install tmux if missing
    info "Installing tmux..."
    sudo apt-get install -y tmux 2>/dev/null || sudo yum install -y tmux 2>/dev/null || true
    if command -v tmux &>/dev/null; then
      tmux new-session -d -s openclaw "openclaw gateway start"
      sleep 2
      success "OpenClaw gateway started (tmux)"
    else
      warn "tmux unavailable. Start gateway manually: openclaw gateway start"
    fi
  fi
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
  echo "    Re-run with --telegram-token=<tok> or --mattermost-token=<tok>"
  echo "    OR edit openclaw.json and run: openclaw gateway restart"
  echo ""
fi
echo "  Next: edit ${OC_WORKSPACE}/SOUL.md to give ${AGENT^} a personality."
echo "  Then: openclaw gateway status"
echo ""
