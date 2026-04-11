#!/usr/bin/env bash
# bootstrap.sh — One-command agent bootstrap from CCC
# Installs OpenClaw, seeds workspace, configures agent identity, starts daemon.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/deploy/bootstrap.sh | \
#     bash -s -- --ccc=http://146.190.134.110:8789 --token=<bootstrap-token> --agent=boris
#
# NOTE: Port 8789 is the OpenClaw gateway (CCC API). Port 8788 is the workqueue dashboard.
# Use 8789 for --ccc. If you have a pre-known agent token, pass --agent-token=<token> to skip
# the bootstrap API call (useful if the API is down or the token is already known).
#
# All secrets (NVIDIA key, Mattermost token, etc.) are fetched automatically
# from CCC via the bootstrap token. No --nvidia-key or channel tokens needed.
set -euo pipefail

CCC=""
TOKEN=""
AGENT=""
AGENT_TOKEN_OVERRIDE=""
# These may be overridden by CLI args, but CCC secrets take precedence if not provided
NVIDIA_KEY=""
TELEGRAM_TOKEN=""
MATTERMOST_TOKEN=""

for arg in "$@"; do
  case "$arg" in
    --ccc=*)               CCC="${arg#--ccc=}"               ;;
    --token=*)             TOKEN="${arg#--token=}"             ;;
    --agent=*)             AGENT="${arg#--agent=}"             ;;
    --agent-token=*)       AGENT_TOKEN_OVERRIDE="${arg#--agent-token=}" ;;
    --nvidia-key=*)        NVIDIA_KEY="${arg#--nvidia-key=}"   ;;
    --telegram-token=*)    TELEGRAM_TOKEN="${arg#--telegram-token=}" ;;
    --mattermost-token=*)  MATTERMOST_TOKEN="${arg#--mattermost-token=}" ;;
  esac
done

if [[ -z "$CCC" || -z "$AGENT" ]]; then
  echo "Usage: bootstrap.sh --ccc=<url> --token=<bootstrap-token> --agent=<name> [--agent-token=<token>]" >&2
  echo "  --token is required unless --agent-token is provided directly." >&2
  exit 1
fi

if [[ -z "$TOKEN" && -z "$AGENT_TOKEN_OVERRIDE" ]]; then
  echo "ERROR: Either --token=<bootstrap-token> or --agent-token=<known-token> is required." >&2
  exit 1
fi

# ── Colors ────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; BLUE='\033[0;34m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info()    { echo -e "${BLUE}→${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
warn()    { echo -e "${YELLOW}⚠${NC} $1"; }
error()   { echo -e "${RED}✗${NC} $1"; exit 1; }

echo ""
echo "🐻 CCC Agent Bootstrap: ${AGENT}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── 0. Clean slate (safe) ────────────────────────────────────────────────
info "Cleaning up previous install..."
pkill -f "openclaw.*gateway" 2>/dev/null || true

# Back up .ccc/.env before wiping — we restore it if bootstrap fails
ENV_BACKUP=""
if [[ -f "$HOME/.ccc/.env" ]]; then
  ENV_BACKUP="$(mktemp /tmp/ccc-env-backup.XXXXXX)"
  cp "$HOME/.ccc/.env" "$ENV_BACKUP"
  info "Backed up existing .env to $ENV_BACKUP"
fi

# Remove old directories but NOT .env backup (we restore below on failure)
rm -rf "$HOME/.openclaw" "$HOME/.ccc" 2>/dev/null || true
success "Clean slate ready"

# Trap: restore .env backup if we exit unexpectedly before step 8
_restore_env_on_failure() {
  local code=$?
  if [[ $code -ne 0 && -n "$ENV_BACKUP" && -f "$ENV_BACKUP" ]]; then
    mkdir -p "$HOME/.ccc"
    cp "$ENV_BACKUP" "$HOME/.ccc/.env"
    chmod 600 "$HOME/.ccc/.env"
    echo "⚠ Bootstrap failed (exit $code) — restored previous .env from backup" >&2
    rm -f "$ENV_BACKUP"
  fi
}
trap _restore_env_on_failure EXIT

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

# ── 2b. Install Hermes Agent (standard runtime) ──────────────────────────
if command -v hermes &>/dev/null; then
  success "Hermes agent already installed ($(hermes --version 2>/dev/null || echo 'version unknown'))"
else
  info "Installing Hermes agent..."
  if command -v pipx &>/dev/null; then
    pipx install hermes-agent 2>/dev/null && success "Hermes installed (pipx)" || true
  fi
  if ! command -v hermes &>/dev/null; then
    pip3 install hermes-agent 2>/dev/null && success "Hermes installed (pip3)" || true
  fi
  export PATH="$HOME/.local/bin:$PATH"
  if command -v hermes &>/dev/null; then
    success "Hermes agent installed"
  else
    warn "Hermes agent install failed — install manually: pipx install hermes-agent"
  fi
fi

# ── 3. Clone / update CCC workspace ──────────────────────────────────────
CCC_WORKSPACE="$HOME/.ccc/workspace"
CLAWFS_CCC_REPO="${CLAWFS_CCC_REPO:-$HOME/clawfs/repos/CCC}"
info "Setting up CCC workspace at $CCC_WORKSPACE..."

# Prefer ClawFS shared repo if available — single copy shared by all agents
if [[ -d "$CLAWFS_CCC_REPO/.git" ]]; then
  info "ClawFS shared repo found at $CLAWFS_CCC_REPO"
  if [[ -L "$CCC_WORKSPACE" ]]; then
    CURRENT_TARGET="$(readlink -f "$CCC_WORKSPACE")"
    CLAWFS_RESOLVED="$(readlink -f "$CLAWFS_CCC_REPO")"
    if [[ "$CURRENT_TARGET" = "$CLAWFS_RESOLVED" ]]; then
      success "Workspace already symlinked to ClawFS repo"
    else
      warn "Workspace symlink points to $CURRENT_TARGET, updating to ClawFS"
      ln -sfn "$CLAWFS_CCC_REPO" "$CCC_WORKSPACE"
      success "Workspace re-symlinked to ClawFS repo"
    fi
  elif [[ -d "$CCC_WORKSPACE" ]]; then
    warn "Workspace is a real directory at $CCC_WORKSPACE"
    warn "ClawFS repo available — consider: rm -rf $CCC_WORKSPACE && ln -s $CLAWFS_CCC_REPO $CCC_WORKSPACE"
    info "Keeping existing directory for now, pulling latest"
    git -C "$CCC_WORKSPACE" pull --ff-only || warn "git pull failed"
  else
    mkdir -p "$(dirname "$CCC_WORKSPACE")"
    ln -s "$CLAWFS_CCC_REPO" "$CCC_WORKSPACE"
    success "Workspace symlinked to ClawFS: $CCC_WORKSPACE -> $CLAWFS_CCC_REPO"
  fi
elif [[ -d "$CCC_WORKSPACE/.git" ]]; then
  # Fallback: no ClawFS, but local clone exists
  warn "ClawFS not available — using local clone, pulling latest"
  git -C "$CCC_WORKSPACE" pull --ff-only || warn "git pull failed"
else
  # Fallback: no ClawFS, no local clone — fresh git clone
  info "ClawFS not available — cloning repo locally"
  git clone "${CCC_REPO:-https://github.com/jordanhubbard/rockyandfriends.git}" "$CCC_WORKSPACE"
fi
success "CCC workspace ready"

# ── 4. Call bootstrap API ─────────────────────────────────────────────────
BOOTSTRAP_JSON=""
REPO_URL=""
AGENT_TOKEN=""
CCC_URL="${CCC}"  # default to the --ccc URL; may be overridden by API response
DEPLOY_KEY=""

if [[ -n "$AGENT_TOKEN_OVERRIDE" ]]; then
  # Skip bootstrap API call — use the provided token directly
  AGENT_TOKEN="$AGENT_TOKEN_OVERRIDE"
  info "Using pre-provided agent token (skipping bootstrap API call)"
  success "Agent token set from --agent-token"
else
  info "Consuming bootstrap token..."
  BOOTSTRAP_RESP=$(curl -sf "${CCC}/api/bootstrap?token=${TOKEN}" 2>&1) || true
  # Verify it looks like JSON (not an error page or empty)
  if echo "$BOOTSTRAP_RESP" | grep -q '"ok":true'; then
    BOOTSTRAP_JSON="$BOOTSTRAP_RESP"
  fi

  if [[ -z "$BOOTSTRAP_JSON" ]]; then
    # If we have a previous .env with a working token, offer to use it
    if [[ -n "$ENV_BACKUP" ]]; then
      PREV_TOKEN=$(grep '^CCC_AGENT_TOKEN=' "$ENV_BACKUP" 2>/dev/null | cut -d= -f2 | tr -d '"' || true)
      if [[ -n "$PREV_TOKEN" ]]; then
        warn "Bootstrap API failed — re-using previous agent token from .env backup"
        warn "To use a fresh token, re-run with a valid --token or --agent-token"
        AGENT_TOKEN="$PREV_TOKEN"
      fi
    fi
    if [[ -z "$AGENT_TOKEN" ]]; then
      error "Bootstrap API call failed or returned invalid response.\nResponse: ${BOOTSTRAP_RESP:-<empty>}\nCheck that CCC is reachable at ${CCC} (port 8788) and the token is valid/unexpired.\nAlternatively, pass --agent-token=<known-token> to skip the API call."
    fi
  else
    # Parse core fields using node (handles nested JSON safely)
    _parse() { node -e "try{const d=JSON.parse(require('fs').readFileSync('/dev/stdin','utf8'));process.stdout.write(String(d${1}||''))}catch(e){}" <<< "$BOOTSTRAP_JSON" 2>/dev/null || true; }

    REPO_URL=$(_parse   ".repoUrl")
    AGENT_TOKEN=$(_parse ".agentToken")
    CCC_URL=$(_parse    ".cccUrl")
    DEPLOY_KEY=$(_parse  ".deployKey")

    if [[ -z "$AGENT_TOKEN" ]]; then
      error "Bootstrap response missing agentToken. Response: ${BOOTSTRAP_JSON}"
    fi
    success "Bootstrap token consumed — agent token issued"
  fi
fi

# ── 4b. Extract secrets from bootstrap response ───────────────────────────
# The bootstrap API returns the full secrets bundle. Extract what we need now
# so openclaw.json can be written correctly in step 7. CLI args take precedence
# if explicitly provided (for testing/overrides); otherwise use CCC secrets.
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

[[ -z "$NVIDIA_KEY"        ]] && NVIDIA_KEY=$(_secret        "NVIDIA_API_KEY"     "nvidia_api_key")
[[ -z "$TOKENHUB_URL"      ]] && TOKENHUB_URL=$(_secret      "TOKENHUB_URL"       "tokenhub_url")
[[ -z "$TOKENHUB_KEY"      ]] && TOKENHUB_KEY=$(_secret      "TOKENHUB_AGENT_KEY" "tokenhub_agent_key")
[[ -z "$MATTERMOST_TOKEN"  ]] && MATTERMOST_TOKEN=$(_secret  "MATTERMOST_TOKEN"   "mattermost_token")
[[ -z "$TELEGRAM_TOKEN"    ]] && TELEGRAM_TOKEN=$(_secret    "TELEGRAM_BOT_TOKEN" "telegram_token")

# Fetch per-agent Slack tokens from CCC (stored as <agent>_slack bundle)
SLACK_BOT_TOKEN=""
SLACK_APP_TOKEN=""
if [[ -n "$AGENT" ]]; then
  SLACK_BUNDLE=$(curl -sf "${CCC_URL}/api/secrets/${AGENT}_slack" \
    -H "Authorization: Bearer ${AGENT_TOKEN}" 2>/dev/null || echo "")
  if [[ -n "$SLACK_BUNDLE" ]]; then
    SLACK_BOT_TOKEN=$(echo "$SLACK_BUNDLE" | node -e "process.stdin.setEncoding('utf8');let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{try{const p=JSON.parse(d);console.log(p.secrets?.SLACK_BOT_TOKEN||'')}catch(e){}}" 2>/dev/null || echo "")
    SLACK_APP_TOKEN=$(echo "$SLACK_BUNDLE" | node -e "process.stdin.setEncoding('utf8');let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{try{const p=JSON.parse(d);console.log(p.secrets?.SLACK_APP_TOKEN||'')}catch(e){}}" 2>/dev/null || echo "")
  fi
fi
if [[ -n "$SLACK_BOT_TOKEN" ]]; then
  success "Slack tokens obtained from CCC secrets (${AGENT}_slack)"
else
  warn "No Slack tokens found in CCC for agent '${AGENT}' — Slack channel will be disabled"
fi

if [[ -n "$NVIDIA_KEY" ]]; then
  success "NVIDIA API key obtained from CCC secrets"
else
  warn "No NVIDIA_API_KEY in CCC secrets — will use anthropic direct (set ANTHROPIC_API_KEY in env)"
fi

# ── 5. Deploy key + SSH config ────────────────────────────────────────────
if [[ -n "$DEPLOY_KEY" ]]; then
  info "Installing deploy key..."
  mkdir -p "$HOME/.ssh"
  printf '%s\n' "$DEPLOY_KEY" > "$HOME/.ssh/ccc-deploy-key"
  chmod 600 "$HOME/.ssh/ccc-deploy-key"
  SSH_CONF="$HOME/.ssh/config"
  if ! grep -q "ccc-deploy-key" "$SSH_CONF" 2>/dev/null; then
    cat >> "$SSH_CONF" <<'SSHEOF'

Host github.com
  IdentityFile ~/.ssh/ccc-deploy-key
  StrictHostKeyChecking no
SSHEOF
    chmod 600 "$SSH_CONF"
  fi
  if [[ -n "$REPO_URL" ]]; then
    git -C "$CCC_WORKSPACE" remote set-url origin "$REPO_URL"
    git -C "$CCC_WORKSPACE" fetch origin || warn "git fetch failed — deploy key may not have read access yet"
  fi
  success "Deploy key installed"
fi

# ── 6. Seed OpenClaw workspace ────────────────────────────────────────────
OC_WORKSPACE="$HOME/.openclaw/workspace"
info "Seeding OpenClaw workspace at $OC_WORKSPACE..."
mkdir -p "$OC_WORKSPACE/memory/people" "$OC_WORKSPACE/skills"

# Copy shared files from repo
SHARED_DIR="$CCC_WORKSPACE/openclaw/shared"
if [[ -d "$SHARED_DIR" ]]; then
  cp "$SHARED_DIR/AGENTS.md" "$OC_WORKSPACE/AGENTS.md" 2>/dev/null || true
fi

# Copy agent soul if it exists; otherwise stub it
SOUL_SRC="$CCC_WORKSPACE/openclaw/souls/${AGENT}.md"
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
- **CCC:** ${CCC_URL}
IDEOF

# Bootstrap MEMORY.md if not present
[[ -f "$OC_WORKSPACE/MEMORY.md" ]] || cat > "$OC_WORKSPACE/MEMORY.md" <<MEMEOF
# MEMORY.md - Long-Term Memory

## Identity
- My name is ${AGENT^}. I am a member of the Rocky & Friends agent crew.
- CCC hub: ${CCC_URL}
MEMEOF

# Bootstrap HEARTBEAT.md
[[ -f "$OC_WORKSPACE/HEARTBEAT.md" ]] || cat > "$OC_WORKSPACE/HEARTBEAT.md" <<HBEOF
# HEARTBEAT.md

# Standard heartbeat. Check queue and CCC health each beat.
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

SLACK_FRAGMENT=""
if [[ -n "$SLACK_BOT_TOKEN" ]]; then
  SLACK_FRAGMENT=$(cat <<SLKEOF
    "slack": {
      "enabled": true,
      "mode": "socket",
      "botToken": "${SLACK_BOT_TOKEN}",
      "appToken": "${SLACK_APP_TOKEN}",
      "streaming": "partial",
      "nativeStreaming": true
    }
SLKEOF
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
  warn "No NVIDIA_API_KEY in secrets or args — defaulting to anthropic direct. Set ANTHROPIC_API_KEY in env or add nvidia/api_key to CCC secrets."
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
    ${SLACK_FRAGMENT}${SLACK_FRAGMENT:+,}${TELEGRAM_FRAGMENT}${TELEGRAM_FRAGMENT:+,}${MATTERMOST_FRAGMENT}
  }
}
OCEOF
success "openclaw.json written"

# ── 8. Write ~/.ccc/.env ──────────────────────────────────────────────────
info "Writing ~/.ccc/.env..."
mkdir -p "$HOME/.ccc"
ENV_FILE="$HOME/.ccc/.env"
touch "$ENV_FILE"
for key in AGENT_NAME CCC_AGENT_TOKEN CCC_URL AGENT_HOST NVIDIA_API_KEY NVIDIA_API_BASE; do
  sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
done
cat >> "$ENV_FILE" <<ENVEOF
AGENT_NAME=${AGENT}
CCC_AGENT_TOKEN=${AGENT_TOKEN}
CCC_URL=${CCC_URL}
AGENT_HOST=$(hostname)
NVIDIA_API_BASE=https://inference-api.nvidia.com/v1
NVIDIA_API_KEY=${NVIDIA_KEY}
# TokenHub — preferred inference router (aggregates local vLLM + NVIDIA NIM)
TOKENHUB_URL=${TOKENHUB_URL:-http://146.190.134.110:8090}
TOKENHUB_AGENT_KEY=${TOKENHUB_KEY}
ENVEOF
chmod 600 "$ENV_FILE"

# ── 8 smoke test: verify critical vars are non-empty ─────────────────────
_env_val() { grep "^${1}=" "$ENV_FILE" 2>/dev/null | cut -d= -f2- | tr -d '"' || true; }
_SMOKE_OK=true
for _VAR in AGENT_NAME CCC_AGENT_TOKEN CCC_URL; do
  _VAL=$(_env_val "$_VAR")
  if [[ -z "$_VAL" ]]; then
    warn "SMOKE TEST FAIL: ${_VAR} is empty in .env — bootstrap may be incomplete"
    _SMOKE_OK=false
  fi
done
if [[ "$_SMOKE_OK" == true ]]; then
  success "~/.ccc/.env written and smoke-tested (all critical vars non-empty)"
else
  # Don't exit — secrets may be written in 8b. But flag it.
  warn "~/.ccc/.env has empty critical vars — check the file before using this agent"
fi

# ── 8b. Write full secrets bundle to .env ────────────────────────────────
# Secrets were already fetched as part of the bootstrap response in step 4b.
# Write all flat string secrets to .env now. Never overwrites identity keys.
info "Writing secrets bundle to .env..."
node -e "
  try {
    const fs = require('fs');
    const d = JSON.parse(fs.readFileSync('/dev/stdin', 'utf8'));
    const s = d.secrets || {};
    const SKIP = new Set(['CCC_AGENT_TOKEN','CCC_URL','AGENT_NAME','AGENT_HOST']);
    const env = fs.existsSync('$ENV_FILE') ? fs.readFileSync('$ENV_FILE', 'utf8') : '';
    const lines = env.split('\n');
    const written = [];
    for (const [k, v] of Object.entries(s)) {
      // Only write flat string values (skip nested objects/alias bundles)
      if (typeof v !== 'string') continue;
      if (SKIP.has(k)) continue;
      // Skip keys with slashes or other chars invalid in shell env var names
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(k)) continue;
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

# ── Set gateway.mode=local (required for agent operation) ─────────────────
openclaw config set gateway.mode local 2>/dev/null || true

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

# ── 9e. Install agentfs-sync ──────────────────────────────────────────────
AGENTFS_BIN="/usr/local/bin/agentfs-sync"
AGENTFS_SVC="/etc/systemd/system/agentfs-sync.service"
AGENTFS_SVC_SRC="$CCC_WORKSPACE.ccc/agentfs-sync/agentfs-sync.service"

if [[ ! -f "$AGENTFS_BIN" ]]; then
  info "Downloading agentfs-sync from MinIO..."
  # Use public endpoint so agents without LAN access can reach it
  _AGENTFS_URL="http://146.190.134.110:9000/agents/shared/bin/agentfs-sync"
  if curl -sf --max-time 30 -o /tmp/agentfs-sync "$_AGENTFS_URL" 2>/dev/null; then
    sudo install -m 755 /tmp/agentfs-sync "$AGENTFS_BIN"
    rm -f /tmp/agentfs-sync
    success "agentfs-sync installed from MinIO"
  else
    warn "agentfs-sync not yet deployed to MinIO — run after first build"
  fi
fi

if [[ -f "$AGENTFS_BIN" ]]; then
  if [[ -f "$AGENTFS_SVC_SRC" ]]; then
    info "Installing agentfs-sync systemd service..."
    mkdir -p "$HOME/.ccc/logs"
    sed "s/AGENT_USER/$(whoami)/g" "$AGENTFS_SVC_SRC" | sudo tee "$AGENTFS_SVC" > /dev/null
    sudo systemctl daemon-reload
    sudo systemctl enable agentfs-sync
    sudo systemctl restart agentfs-sync 2>/dev/null || sudo systemctl start agentfs-sync 2>/dev/null || true
    success "agentfs-sync service enabled and started"
  else
    warn "agentfs-sync service template not found in workspace — skipping service install"
  fi
fi

# ── 9f. Install openclaw-register service ────────────────────────────────
REGISTER_SVC="/etc/systemd/system/openclaw-register.service"
REGISTER_SVC_SRC="$CCC_WORKSPACE.ccc/scripts/openclaw-register.service"

if [[ -f "$REGISTER_SVC_SRC" ]]; then
  info "Installing openclaw-register systemd service..."
  mkdir -p "$HOME/.ccc/logs"
  sed "s/AGENT_USER/$(whoami)/g" "$REGISTER_SVC_SRC" | sudo tee "$REGISTER_SVC" > /dev/null
  sudo systemctl daemon-reload
  sudo systemctl enable openclaw-register
  sudo systemctl restart openclaw-register 2>/dev/null || sudo systemctl start openclaw-register 2>/dev/null || true
  success "openclaw-register service enabled and started"
else
  warn "openclaw-register.service not found in workspace — skipping (run after pulling latest)"
fi

# ── 9g. ClawFS / JuiceFS mount ────────────────────────────────────────────
CLAWFS_MOUNT="${HOME}/clawfs"
CLAWFS_REDIS="redis://100.89.199.14:6379/1"
CLAWFS_CACHE="/tmp/jfscache"

_clawfs_mounted() { [[ -f "${CLAWFS_MOUNT}/.config" ]]; }

if [[ "$(uname)" == "Linux" ]]; then
  info "Setting up ClawFS (JuiceFS FUSE mount)..."
  # Install JuiceFS if not present
  if ! command -v juicefs &>/dev/null; then
    JFS_VER="1.2.2"
    JFS_URL="https://github.com/juicedata/juicefs/releases/download/v${JFS_VER}/juicefs-${JFS_VER}-linux-amd64.tar.gz"
    if curl -sfL "$JFS_URL" | tar xz -C /tmp juicefs 2>/dev/null; then
      sudo install -m 755 /tmp/juicefs /usr/local/bin/juicefs
      rm -f /tmp/juicefs
      success "JuiceFS ${JFS_VER} installed"
    else
      warn "JuiceFS download failed — ClawFS will not be available"
    fi
  fi
  # Install FUSE utils if missing
  if ! command -v fusermount &>/dev/null && ! command -v fusermount3 &>/dev/null; then
    sudo apt-get install -y -q fuse3 2>/dev/null || sudo yum install -y -q fuse3 2>/dev/null || true
  fi
  # Mount ClawFS
  if command -v juicefs &>/dev/null; then
    mkdir -p "$CLAWFS_MOUNT" "$CLAWFS_CACHE"
    if ! _clawfs_mounted; then
      juicefs mount --background --cache-dir "$CLAWFS_CACHE" "$CLAWFS_REDIS" "$CLAWFS_MOUNT" 2>/dev/null && \
        success "ClawFS mounted at $CLAWFS_MOUNT" || \
        warn "ClawFS mount failed — models will be downloaded locally"
    else
      success "ClawFS already mounted at $CLAWFS_MOUNT"
    fi
    # Install systemd unit for persistence
    CLAWFS_SVC="/etc/systemd/system/clawfs-${AGENT}.service"
    if [[ ! -f "$CLAWFS_SVC" ]] && command -v systemctl &>/dev/null; then
      cat > /tmp/clawfs.service <<CFSEOF
[Unit]
Description=ClawFS JuiceFS mount for ${AGENT}
After=network-online.target
Wants=network-online.target

[Service]
Type=forking
User=$(whoami)
ExecStart=/usr/local/bin/juicefs mount --background --cache-dir ${CLAWFS_CACHE} ${CLAWFS_REDIS} ${CLAWFS_MOUNT}
ExecStop=/usr/local/bin/juicefs umount ${CLAWFS_MOUNT}
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
CFSEOF
      sudo mv /tmp/clawfs.service "$CLAWFS_SVC"
      sudo systemctl daemon-reload
      sudo systemctl enable "clawfs-${AGENT}" 2>/dev/null || true
      success "ClawFS systemd service installed"
    fi
  fi
elif [[ "$(uname)" == "Darwin" ]]; then
  if command -v juicefs &>/dev/null; then
    mkdir -p "$CLAWFS_MOUNT" "$CLAWFS_CACHE"
    if ! _clawfs_mounted; then
      juicefs mount --background --cache-dir "$CLAWFS_CACHE" "$CLAWFS_REDIS" "$CLAWFS_MOUNT" 2>/dev/null && \
        success "ClawFS mounted at $CLAWFS_MOUNT" || \
        warn "ClawFS mount failed — install macFUSE first: brew install --cask macfuse (requires reboot)"
    else
      success "ClawFS already mounted at $CLAWFS_MOUNT"
    fi
  else
    warn "JuiceFS not found — to enable ClawFS on macOS:"
    warn "  1. brew install --cask macfuse   (reboot + approve system extension)"
    warn "  2. brew install juicefs"
    warn "  3. juicefs mount --background --cache-dir /tmp/jfscache redis://100.89.199.14:6379/1 ~/clawfs"
  fi
fi

# Write ClawFS vars to .env
for key in CLAWFS_ENABLED CLAWFS_MOUNT CLAWFS_REDIS_URL CLAWFS_CACHE_DIR; do
  sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
done
cat >> "$ENV_FILE" <<CLAWFSENV
CLAWFS_ENABLED=$(_clawfs_mounted && echo true || echo false)
CLAWFS_MOUNT=${CLAWFS_MOUNT}
CLAWFS_REDIS_URL=${CLAWFS_REDIS}
CLAWFS_CACHE_DIR=${CLAWFS_CACHE}
CLAWFSENV

# ── 9h. vLLM setup (GPU nodes only) ──────────────────────────────────────
if [[ ${GPU_COUNT:-0} -gt 0 ]]; then
  info "GPU detected — setting up vLLM model serving..."
  VLLM_MODEL="${VLLM_MODEL:-google/gemma-4-31B-it}"
  VLLM_SERVED_NAME="${VLLM_SERVED_NAME:-gemma}"
  VLLM_PORT="${VLLM_PORT:-8000}"

  # Determine model path: ClawFS > local > will download
  if _clawfs_mounted && [[ -d "${CLAWFS_MOUNT}/models/$(basename $VLLM_MODEL)" ]]; then
    VLLM_MODEL_PATH="${CLAWFS_MOUNT}/models/$(basename $VLLM_MODEL)"
    success "Model found in ClawFS: $VLLM_MODEL_PATH"
  elif [[ -d "$HOME/models/$(basename $VLLM_MODEL)" ]]; then
    VLLM_MODEL_PATH="$HOME/models/$(basename $VLLM_MODEL)"
    success "Model found locally: $VLLM_MODEL_PATH"
  else
    VLLM_MODEL_PATH="$VLLM_MODEL"
    info "Model not cached — vLLM will download from HuggingFace on first start"
  fi

  # Install vLLM if not present
  if ! command -v vllm &>/dev/null && ! python3 -c "import vllm" 2>/dev/null; then
    info "Installing vLLM..."
    pip3 install vllm 2>/dev/null && success "vLLM installed" || warn "vLLM install failed — install manually: pip3 install vllm"
  fi

  # Determine extra args (tensor parallel for multi-GPU)
  VLLM_EXTRA_ARGS="${VLLM_EXTRA_ARGS:-}"
  if [[ ${GPU_COUNT} -gt 1 && -z "$VLLM_EXTRA_ARGS" ]]; then
    VLLM_EXTRA_ARGS="--tensor-parallel-size ${GPU_COUNT}"
    info "Multi-GPU detected: adding $VLLM_EXTRA_ARGS"
  fi

  # Write vLLM vars to .env
  for key in VLLM_ENABLED VLLM_MODEL VLLM_SERVED_NAME VLLM_PORT VLLM_MODEL_PATH VLLM_EXTRA_ARGS; do
    sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
  done
  cat >> "$ENV_FILE" <<VLLMENV
VLLM_ENABLED=true
VLLM_MODEL=${VLLM_MODEL}
VLLM_SERVED_NAME=${VLLM_SERVED_NAME}
VLLM_PORT=${VLLM_PORT}
VLLM_MODEL_PATH=${VLLM_MODEL_PATH}
VLLM_EXTRA_ARGS=${VLLM_EXTRA_ARGS}
VLLMENV

  # Start vLLM
  if python3 -c "import vllm" 2>/dev/null; then
    _vllm_running() { curl -sf "http://127.0.0.1:${VLLM_PORT}/v1/models" > /dev/null 2>&1; }

    if _vllm_running; then
      success "vLLM already running on port ${VLLM_PORT}"
    elif command -v systemctl &>/dev/null && [[ "$(uname)" == "Linux" ]]; then
      # Install systemd unit
      VLLM_SVC="/etc/systemd/system/vllm-${AGENT}.service"
      cat > /tmp/vllm.service <<VLLMSVC
[Unit]
Description=vLLM model server for ${AGENT}
After=network-online.target clawfs-${AGENT}.service
Wants=network-online.target

[Service]
Type=simple
User=$(whoami)
Environment="HOME=${HOME}"
ExecStart=/usr/bin/python3 -m vllm.entrypoints.openai.api_server --model ${VLLM_MODEL_PATH} --served-model-name ${VLLM_SERVED_NAME} --port ${VLLM_PORT} ${VLLM_EXTRA_ARGS}
Restart=on-failure
RestartSec=15

[Install]
WantedBy=multi-user.target
VLLMSVC
      sudo mv /tmp/vllm.service "$VLLM_SVC"
      sudo systemctl daemon-reload
      sudo systemctl enable "vllm-${AGENT}" 2>/dev/null || true
      sudo systemctl start "vllm-${AGENT}" 2>/dev/null || true
      success "vLLM systemd service installed and started"
    else
      # tmux fallback
      tmux kill-session -t vllm 2>/dev/null || true
      tmux new-session -d -s vllm "python3 -m vllm.entrypoints.openai.api_server --model ${VLLM_MODEL_PATH} --served-model-name ${VLLM_SERVED_NAME} --port ${VLLM_PORT} ${VLLM_EXTRA_ARGS}"
      success "vLLM started (tmux session 'vllm')"
    fi
  fi
else
  info "No GPU detected — skipping vLLM setup"
  for key in VLLM_ENABLED VLLM_MODEL VLLM_SERVED_NAME VLLM_PORT VLLM_MODEL_PATH VLLM_EXTRA_ARGS; do
    sed -i "/^${key}=/d" "$ENV_FILE" 2>/dev/null || true
  done
  echo "VLLM_ENABLED=false" >> "$ENV_FILE"
fi

# ── 9i. Install Hermes skills + migrate OpenClaw ─────────────────────────
if command -v hermes &>/dev/null; then
  info "Configuring Hermes agent..."
  mkdir -p "$HOME/.hermes/skills"
  # Install ccc-node skill
  if [[ -d "$CCC_WORKSPACE/skills/ccc-node" ]]; then
    cp -r "$CCC_WORKSPACE/skills/ccc-node/" "$HOME/.hermes/skills/ccc-node/"
    success "CCC-node skill installed into Hermes"
  fi
  # Migrate OpenClaw config if present
  if [[ -d "$HOME/.openclaw" ]]; then
    hermes claw migrate 2>/dev/null && success "OpenClaw config migrated to Hermes" || true
  fi
fi

# ── 10. Hardware fingerprint + heartbeat ─────────────────────────────────
info "Collecting hardware fingerprint..."

# GPU info via nvidia-smi
GPU_COUNT=0
GPU_MODEL=""
GPU_VRAM_GB=0
if command -v nvidia-smi &>/dev/null; then
  GPU_COUNT=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | wc -l || echo 0)
  GPU_MODEL=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1 | tr -d '\n' || echo "")
  GPU_VRAM_MB=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | \
    awk '{s+=$1} END {print int(s)}' || echo 0)
  GPU_VRAM_GB=$(( GPU_VRAM_MB / 1024 ))
fi

# CPU info
CPU_CORES=$(nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo 2>/dev/null || echo 0)
CPU_MODEL=$(grep 'model name' /proc/cpuinfo 2>/dev/null | head -1 | cut -d: -f2 | sed 's/^ *//' || echo "unknown")
CPU_ARCH=$(uname -m 2>/dev/null || echo "unknown")

# RAM
RAM_GB=0
if [[ -r /proc/meminfo ]]; then
  RAM_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
  RAM_GB=$(( RAM_KB / 1024 / 1024 ))
fi

# Disk free on home
DISK_FREE_GB=$(df -BG "$HOME" 2>/dev/null | tail -1 | awk '{print $4}' | tr -d 'G' || echo 0)

HW_JSON=$(cat <<HWEOF
{
  "gpu_count": ${GPU_COUNT},
  "gpu_model": "${GPU_MODEL}",
  "gpu_vram_gb": ${GPU_VRAM_GB},
  "cpu_cores": ${CPU_CORES},
  "cpu_model": "${CPU_MODEL}",
  "cpu_arch": "${CPU_ARCH}",
  "ram_gb": ${RAM_GB},
  "disk_free_gb": ${DISK_FREE_GB}
}
HWEOF
)

info "Hardware: ${GPU_COUNT}x ${GPU_MODEL:-none} (${GPU_VRAM_GB}GB VRAM), ${CPU_CORES}x CPU, ${RAM_GB}GB RAM"

info "Posting heartbeat + hardware fingerprint to CCC..."
curl -s -X POST "${CCC_URL}/api/heartbeat/${AGENT}" \
  -H "Authorization: Bearer ${AGENT_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{
    \"agent\":\"${AGENT}\",
    \"host\":\"$(hostname)\",
    \"status\":\"online\",
    \"version\":\"bootstrap\",
    \"hardware\":${HW_JSON}
  }" > /dev/null || warn "Heartbeat post failed (non-fatal)"

# Also PATCH agent record with real hardware data so CCC dashboard is accurate
curl -s -X PATCH "${CCC_URL}/api/agents/${AGENT}" \
  -H "Authorization: Bearer ${AGENT_TOKEN}" \
  -H "Content-Type: application/json" \
  -d "{
    \"capabilities\": {
      \"gpu\": $([ ${GPU_COUNT} -gt 0 ] && echo true || echo false),
      \"gpu_model\": \"${GPU_MODEL}\",
      \"gpu_count\": ${GPU_COUNT},
      \"gpu_vram_gb\": ${GPU_VRAM_GB},
      \"cpu_cores\": ${CPU_CORES},
      \"cpu_model\": \"${CPU_MODEL}\",
      \"cpu_arch\": \"${CPU_ARCH}\",
      \"ram_gb\": ${RAM_GB}
    }
  }" > /dev/null || warn "Capabilities PATCH failed (non-fatal — dashboard may show stale hw info)"

success "Heartbeat + hardware fingerprint posted"

# ── Write onboarding signature ────────────────────────────────────────────
_AGENT_JSON="$HOME/.ccc/agent.json"
if [ ! -f "$_AGENT_JSON" ]; then
  _CCC_VERSION=$(cd "${CCC_WORKSPACE:-$HOME/.ccc/workspace}" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")
  _NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  if command -v node >/dev/null 2>&1; then
    node -e "
      require('fs').writeFileSync('$_AGENT_JSON', JSON.stringify({
        schema_version: 1,
        agent_name: '${AGENT:-unknown}',
        host: '$(hostname)',
        onboarded_at: '$_NOW',
        onboarded_by: 'bootstrap.sh',
        ccc_version: '$_CCC_VERSION',
        last_upgraded_at: '$_NOW',
        last_upgraded_version: '$_CCC_VERSION'
      }, null, 2) + '\n');
    " && chmod 600 "$_AGENT_JSON" && success "Onboarding signature written to $_AGENT_JSON" \
      || warn "Failed to write agent.json (non-fatal)"
  else
    warn "node not found — skipping agent.json write"
  fi
else
  info "agent.json already exists at $_AGENT_JSON"
fi

# ── 11. Done ──────────────────────────────────────────────────────────────
# Clear the failure trap — we succeeded
trap - EXIT
[[ -n "${ENV_BACKUP:-}" && -f "${ENV_BACKUP:-/dev/null}" ]] && rm -f "$ENV_BACKUP"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${GREEN}✅ Bootstrap complete!${NC} ${AGENT^} is alive."
echo ""
echo "  OpenClaw workspace:  ${OC_WORKSPACE}"
echo "  OpenClaw config:     ${OC_CONFIG}"
echo "  CCC workspace:       ${CCC_WORKSPACE}"
echo "  CCC env:             ${HOME}/.ccc/.env"
echo ""
if [[ -z "$TELEGRAM_TOKEN" && -z "$MATTERMOST_TOKEN" ]]; then
  echo -e "${YELLOW}  ⚠ No messaging channels configured.${NC}"
  echo "    Add TELEGRAM_BOT_TOKEN or MATTERMOST_TOKEN to CCC secrets and re-bootstrap,"
  echo "    OR edit openclaw.json and run: openclaw gateway restart"
  echo ""
fi
echo "  Next: edit ${OC_WORKSPACE}/SOUL.md to give ${AGENT^} a personality."
echo "  Then: openclaw gateway status"
echo ""
