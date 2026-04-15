#!/bin/bash
# setup-node.sh — Bootstrap a new CCC agent node
# Run once on a new machine. Safe to re-run (idempotent).
#
# Usage:
#   REPO_URL=git@github.com:yourorg/your-ccc-repo.git bash deploy/setup-node.sh
#   OR (from inside an existing clone): bash deploy/setup-node.sh
#
# Tip: run deploy/ccc-init.sh after this to configure your .env interactively.

set -e

REPO_URL="${REPO_URL:-${REPO_URL:-git@github.com:<your-org>/rockyandfriends.git}}"
CCC_DIR="$HOME/.ccc"
WORKSPACE="$CCC_DIR/workspace"
ENV_FILE="$CCC_DIR/.env"
LOG_DIR="$CCC_DIR/logs"
PULL_SCRIPT="$WORKSPACE/deploy/agent-pull.sh"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()    { echo -e "${BLUE}[setup]${NC} $1"; }
success() { echo -e "${GREEN}[setup]${NC} ✓ $1"; }
warn()    { echo -e "${YELLOW}[setup]${NC} ⚠ $1"; }
error()   { echo -e "${RED}[setup]${NC} ✗ $1"; exit 1; }

echo ""
echo "🐿️  CCC Agent Node Setup"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── Detect platform ────────────────────────────────────────────────────────
PLATFORM="unknown"
if [[ "$(uname)" == "Darwin" ]]; then
  PLATFORM="macos"
elif [[ "$(uname)" == "Linux" ]]; then
  PLATFORM="linux"
fi
info "Platform: $PLATFORM"

# ── Check required tools ───────────────────────────────────────────────────
info "Checking required tools..."
for tool in git node curl; do
  if ! command -v $tool &>/dev/null; then
    error "$tool is required but not found. Please install it first."
  fi
done
success "Required tools present (git, node, curl)"

# ── Create directories ─────────────────────────────────────────────────────
mkdir -p "$CCC_DIR" "$LOG_DIR"
success "Created $CCC_DIR"

# ── Clone or update repo ───────────────────────────────────────────────────
if [ -d "$WORKSPACE/.git" ]; then
  info "Repo already exists at $WORKSPACE — pulling latest..."
  cd "$WORKSPACE" && git pull --ff-only
  success "Repo updated"
else
  info "Cloning repo to $WORKSPACE..."
  git clone "$REPO_URL" "$WORKSPACE"
  success "Repo cloned"
fi

# ── Set up .env ────────────────────────────────────────────────────────────
if [ -f "$ENV_FILE" ]; then
  warn ".env already exists at $ENV_FILE — skipping (not overwriting)"
else
  info "Creating .env from template..."
  cp "$WORKSPACE/deploy/.env.template" "$ENV_FILE"
  chmod 600 "$ENV_FILE"
  echo ""
  echo "  ┌─────────────────────────────────────────────────────┐"
  echo "  │  .env created at: $ENV_FILE"
  echo "  │  IMPORTANT: Edit it and fill in your values!"
  echo "  │  Required: AGENT_NAME, AGENT_HOST, CCC_URL"
  echo "  │            CCC_AGENT_TOKEN, NVIDIA_API_KEY"
  echo "  └─────────────────────────────────────────────────────┘"
  echo ""
  warn "Edit $ENV_FILE before running the agent!"
fi

# ── Install pull cron ──────────────────────────────────────────────────────
install_cron_linux() {
  CRON_CMD="*/10 * * * * bash $PULL_SCRIPT >> $LOG_DIR/pull.log 2>&1"
  if crontab -l 2>/dev/null | grep -q "agent-pull.sh"; then
    warn "Pull cron already installed — skipping"
  else
    (crontab -l 2>/dev/null; echo "$CRON_CMD") | crontab -
    success "Pull cron installed (every 10 minutes)"
  fi
}

install_cron_macos() {
  PLIST_SRC="$WORKSPACE/deploy/launchd/com.ccc.agent.plist"
  PLIST_DST="$HOME/Library/LaunchAgents/com.ccc.agent.plist"
  if [ -f "$PLIST_DST" ]; then
    warn "LaunchAgent already installed at $PLIST_DST — reloading"
    launchctl unload "$PLIST_DST" 2>/dev/null || true
  fi
  # Substitute real paths in plist
  sed "s|PULL_SCRIPT_PATH|$PULL_SCRIPT|g; s|LOG_PATH|$LOG_DIR/pull.log|g" \
    "$PLIST_SRC" > "$PLIST_DST"
  launchctl load "$PLIST_DST"
  success "LaunchAgent installed and loaded"
}

info "Installing pull cron..."
if [[ "$PLATFORM" == "linux" ]]; then
  install_cron_linux
elif [[ "$PLATFORM" == "macos" ]]; then
  install_cron_macos
else
  warn "Unknown platform — install cron manually: bash $PULL_SCRIPT"
fi

# ── Install ops crons (watchdog, nudge, memory snapshot) ──────────────────
install_ops_crons() {
  local CRON_FRAGMENT="$WORKSPACE/deploy/crontab-ccc.txt"
  if [ ! -f "$CRON_FRAGMENT" ]; then
    warn "Ops cron fragment not found at $CRON_FRAGMENT — skipping"
    return
  fi
  # Check if already installed (look for a sentinel)
  if crontab -l 2>/dev/null | grep -q "ccc-api-watchdog.mjs"; then
    warn "Ops crons already installed — skipping"
    return
  fi
  # Expand WORKSPACE and LOG_DIR placeholders, then append to crontab
  local EXPANDED
  EXPANDED=$(sed "s|WORKSPACE|$WORKSPACE|g; s|LOG_DIR|$LOG_DIR|g" "$CRON_FRAGMENT" | grep -v '^#' | grep -v '^$')
  (crontab -l 2>/dev/null; echo "$EXPANDED") | crontab -
  success "Ops crons installed (watchdog every 10min, nudge daily 9AM, memory-commit daily midnight)"
}

info "Installing ops crons..."
if [[ "$PLATFORM" == "linux" ]] || [[ "$PLATFORM" == "macos" ]]; then
  install_ops_crons
else
  warn "Unknown platform — install ops crons manually from deploy/crontab-ccc.txt"
fi

# ── Install node dependencies ─────────────────────────────────────────────
info "Installing dashboard dependencies..."
cd "$WORKSPACE/dashboard" && npm install --silent
success "Dependencies installed"

# ── Install coding CLI turbocharger ───────────────────────────────────────
echo ""
info "Checking coding CLI turbocharger..."

# tmux (required for claude-worker.mjs delegation)
if ! command -v tmux &>/dev/null; then
  info "Installing tmux..."
  if [[ "$PLATFORM" == "linux" ]]; then
    sudo apt-get install -y tmux 2>/dev/null || \
    sudo yum install -y tmux 2>/dev/null || \
    warn "Could not auto-install tmux. Please run: sudo apt-get install -y tmux"
  elif [[ "$PLATFORM" == "macos" ]]; then
    brew install tmux 2>/dev/null || \
    warn "Could not auto-install tmux. Please run: brew install tmux"
  fi
else
  success "tmux present ($(tmux -V))"
fi

# Claude Code CLI (primary coding executor)
if command -v claude &>/dev/null; then
  success "Claude Code CLI present ($(claude --version 2>/dev/null | head -1))"
else
  warn "Claude Code CLI not found."
  echo ""
  echo "  ┌─────────────────────────────────────────────────────────────┐"
  echo "  │  RECOMMENDED: Install a coding CLI for the turbocharger     │"
  echo "  │                                                              │"
  echo "  │  Claude Code:  npm install -g @anthropic-ai/claude-code     │"
  echo "  │  Codex:        npm install -g @openai/codex                 │"
  echo "  │  OpenCode:     https://opencode.ai                          │"
  echo "  │                                                              │"
  echo "  │  After install, start a persistent session:                 │"
  echo "  │    tmux new-session -d -s claude-main                       │"
  echo "  │    tmux send-keys -t claude-main \"claude\" Enter             │"
  echo "  │                                                              │"
  echo "  │  Without this, claude_cli work items won't execute locally. │"
  echo "  └─────────────────────────────────────────────────────────────┘"
  echo ""
fi

# ── Install Hermes agent runtime ─────────────────────────────────────────
info "Checking Hermes agent runtime..."

HERMES_INSTALLED=false

if command -v hermes &>/dev/null; then
  HERMES_INSTALLED=true
  success "Hermes agent present ($(hermes --version 2>/dev/null | head -1))"
else
  info "Hermes not found — installing..."
  HERMES_INSTALL_OK=false
  if command -v pipx &>/dev/null; then
    # pipx is the cleanest path (avoids PEP 668 on modern macOS/Linux)
    pipx install hermes-agent --quiet 2>/dev/null && HERMES_INSTALL_OK=true || true
  elif command -v pip3 &>/dev/null || command -v pip &>/dev/null; then
    PIP="$(command -v pip3 || command -v pip)"
    if [[ "$PLATFORM" == "macos" ]]; then
      # PEP 668: Homebrew Python refuses bare pip install on macOS 3.11+
      HERMES_VENV="$HOME/.hermes-install-venv"
      python3 -m venv "$HERMES_VENV" --quiet 2>/dev/null && \
        "$HERMES_VENV/bin/pip" install --quiet hermes-agent 2>/dev/null && \
        ln -sf "$HERMES_VENV/bin/hermes" "$HOME/.local/bin/hermes" 2>/dev/null && \
        HERMES_INSTALL_OK=true || true
    else
      "$PIP" install --quiet hermes-agent 2>/dev/null && HERMES_INSTALL_OK=true || true
    fi
  fi

  if [ "$HERMES_INSTALL_OK" = true ] && command -v hermes &>/dev/null; then
    HERMES_INSTALLED=true
    success "Hermes agent installed ($(hermes --version 2>/dev/null | head -1))"
  else
    warn "Could not auto-install Hermes."
    echo ""
    echo "  ┌──────────────────────────────────────────────────────────────┐"
    echo "  │  Install Hermes manually:                                    │"
    echo "  │    macOS:  pipx install hermes-agent   (preferred)           │"
    echo "  │            brew install pipx && pipx install hermes-agent    │"
    echo "  │    Linux:  pip3 install hermes-agent                         │"
    echo "  │  Then re-run this script to complete setup.                  │"
    echo "  └──────────────────────────────────────────────────────────────┘"
    echo ""
  fi
fi

# Install ccc-node skill into Hermes
CCC_SKILL_SRC="$WORKSPACE/skills/ccc-node"
if [ -d "$CCC_SKILL_SRC" ] && [ "$HERMES_INSTALLED" = true ]; then
  SKILL_DEST="$HOME/.hermes/skills/ccc-node"
  if [ ! -d "$SKILL_DEST" ]; then
    cp -r "$CCC_SKILL_SRC" "$SKILL_DEST"
    success "ccc-node skill installed into Hermes"
  else
    success "ccc-node skill already in Hermes"
  fi
fi

# ── Seed hermes MEMORY.md with CCC fleet context ────────────────────────
# Seeds a typed-network MEMORY.md so a fresh hermes agent knows fleet
# conventions without stumbling across AGENTS.md on its own.
#
# Memory schema (borrowed from agentic-memory concepts):
#   World Knowledge  — verified, stable facts
#   Beliefs          — heuristics with confidence (0.4=tentative, 0.8+=strong)
#                      decay: -0.1 on contradiction, prune below 0.2
#   Experiences      — work session outcomes (success/failure/mixed)
#   Reflections      — synthesized cross-session patterns
#   Entities         — profiles of nodes, services, and systems
seed_hermes_memory() {
  local HERMES_DIR="$HOME/.hermes"
  local MEMORY_FILE="$HERMES_DIR/MEMORY.md"
  # Two sentinels for idempotency: old flat format and new typed format
  local SENTINEL_OLD="## CCC Fleet Context"
  local SENTINEL_NEW="<!-- ccc-fleet-seed -->"

  if ! command -v hermes &>/dev/null; then return; fi
  if grep -qE "(## CCC Fleet Context|<!-- ccc-fleet-seed -->)" "$MEMORY_FILE" 2>/dev/null; then
    success "CCC fleet context already in hermes MEMORY.md — skipping"
    return
  fi

  mkdir -p "$HERMES_DIR"
  cat >> "$MEMORY_FILE" <<'HERMESMEM'
<!-- ccc-fleet-seed: written by setup-node.sh — idempotent, do not remove -->
<!--
  MEMORY SCHEMA
  World Knowledge : verified, objective facts (stable)
  Beliefs         : heuristics + confidence score + updated date
                    confidence: 0.4=tentative, 0.6=moderate, 0.8+=strong
                    decay: -0.1 on contradiction; prune entries below 0.2
  Experiences     : session outcomes — what was tried, outcome, lessons
  Reflections     : synthesized patterns derived from multiple experiences
  Entities        : profiles of fleet nodes, services, and key systems
-->

## World Knowledge

### CCC Fleet
- This agent is part of the CCC fleet managed by ~/Src/CCC (rockyandfriends repo)
- Full workspace docs live in ~/Src/CCC/AGENTS.md — read on first boot
- Config: ~/.ccc/.env — CCC_URL and CCC_AGENT_TOKEN are required before starting
- Workqueue: ~/Src/CCC/workqueue/queue.json — polled each heartbeat cycle
- Secrets are stored in TokenHub (~/Src/tokenhub); never hardcode credentials

### Companion Repos
Clone these on any new command node without waiting to be asked:
- ~/Src/rockyandfriends  — core fleet repo (already present)
- ~/Src/tokenhub         — required: LLM router and secret store
- ~/Src/nanolang         — dev nodes only
- ~/Src/agentos          — dev nodes only

Minimum viable command node (no GPU, no dev): rockyandfriends + tokenhub.

## Beliefs

### Fleet Operational Conventions
<!-- confidence: 0.8 | updated: SEED_DATE -->
- Register every secret in TokenHub BEFORE starting any service
<!-- confidence: 0.8 | updated: SEED_DATE -->
- Session startup order: SOUL.md → USER.md → memory/YYYY-MM-DD.md → MEMORY.md (main session only)
<!-- confidence: 0.7 | updated: SEED_DATE -->
- New command nodes need rockyandfriends + tokenhub as a minimum before any other work

## Experiences

## Reflections

## Entities

### Fleet Nodes
<!-- Add node profiles here as you encounter them: name, role, capabilities, quirks -->
HERMESMEM

  # Stamp the actual seed date into the template placeholders
  local SEED_DATE
  SEED_DATE=$(date +%Y-%m-%d)
  if command -v sed &>/dev/null; then
    sed -i.bak "s/SEED_DATE/$SEED_DATE/g" "$MEMORY_FILE" && rm -f "${MEMORY_FILE}.bak"
  fi

  success "CCC fleet context seeded into $MEMORY_FILE (typed-network schema)"
}

info "Seeding hermes fleet context..."
seed_hermes_memory

# ── vLLM (local GPU model serving) ──────────────────────────────────────
if command -v nvidia-smi &>/dev/null; then
  GPU_INFO=$(nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>/dev/null | head -1)
  success "GPU detected: $GPU_INFO"
  if python3 -c "import vllm" 2>/dev/null || command -v vllm &>/dev/null; then
    success "vLLM installed"
    if curl -sf "http://127.0.0.1:${VLLM_PORT:-8000}/v1/models" > /dev/null 2>&1; then
      success "vLLM already running on port ${VLLM_PORT:-8000}"
    else
      info "vLLM installed but not running — bootstrap.sh or systemd will start it"
    fi
  else
    warn "vLLM not installed — bootstrap.sh will install it, or run: pip3 install vllm"
  fi
else
  info "No GPU detected — vLLM not applicable"
fi

# ── Set up systemd service (Linux only) ───────────────────────────────────
if [[ "$PLATFORM" == "linux" ]] && command -v systemctl &>/dev/null; then
  SERVICE_SRC="$WORKSPACE/deploy/systemd/ccc-agent.service"
  SERVICE_DST="/etc/systemd/system/ccc-agent.service"
  if [ -f "$SERVICE_DST" ]; then
    warn "ccc-agent.service already installed — skipping"
  else
    if [ -w "/etc/systemd/system" ] || command -v sudo &>/dev/null; then
      sed "s|AGENT_USER|$(whoami)|g; s|AGENT_HOME|$HOME|g" "$SERVICE_SRC" | sudo tee "$SERVICE_DST" > /dev/null && \
      sudo systemctl daemon-reload && \
      sudo systemctl enable ccc-agent && \
      sudo systemctl start ccc-agent && \
      success "ccc-agent systemd service installed and started" || \
      warn "Could not install systemd service (needs sudo). Run manually: sudo systemctl enable --now ccc-agent"
    else
      warn "No sudo access — install systemd service manually"
    fi
  fi
fi

# ── Write onboarding signature ────────────────────────────────────────────
if [ ! -f "$CCC_DIR/agent.json" ]; then
  CCC_VERSION=$(cd "$WORKSPACE" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")
  _AGENT_BIN="${CCC_AGENT:-$CCC_DIR/bin/ccc-agent}"
  [ ! -x "$_AGENT_BIN" ] && _AGENT_BIN="$(command -v ccc-agent 2>/dev/null || echo "")"

  if [ -x "$_AGENT_BIN" ]; then
    "$_AGENT_BIN" agent init "$CCC_DIR/agent.json" \
      --name="${AGENT_NAME:-unknown}" \
      --host="$(hostname)" \
      --version="$CCC_VERSION" \
      --by="setup-node.sh" \
      && success "Onboarding signature written to $CCC_DIR/agent.json" \
      || warn "Failed to write agent.json (non-fatal)"
  elif command -v python3 >/dev/null 2>&1; then
    python3 - "$CCC_DIR/agent.json" "${AGENT_NAME:-unknown}" "$(hostname)" "$CCC_VERSION" << 'PYEOF'
import json, sys, os
from datetime import datetime, timezone
path, name, host, ver = sys.argv[1:5]
now = datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, 'w') as f:
    json.dump({'schema_version':1,'agent_name':name,'host':host,
               'onboarded_at':now,'onboarded_by':'setup-node.sh',
               'ccc_version':ver,'last_upgraded_at':now,'last_upgraded_version':ver}, f, indent=2)
    f.write('\n')
os.chmod(path, 0o600)
PYEOF
    success "Onboarding signature written to $CCC_DIR/agent.json"
  else
    warn "Neither ccc-agent nor python3 found — skipping agent.json write"
  fi
else
  info "agent.json already exists — skipping (run upgrade-node.sh to update)"
fi

# ── Summary ───────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "🐿️  ${GREEN}Setup complete!${NC}"
echo ""
echo "  Workspace:  $WORKSPACE"
echo "  Config:     $ENV_FILE"
echo "  Logs:       $LOG_DIR"
echo ""
echo "  Next steps:"
echo "  1. Edit $ENV_FILE with your agent's credentials"
echo "  2. Run a manual pull: bash $PULL_SCRIPT"
echo "  3. Check logs: tail -f $LOG_DIR/pull.log"
echo "  4. Start agent runtime: hermes gateway"
echo ""
echo "  To register this agent with CCC:"
echo "  bash $WORKSPACE/deploy/register-agent.sh"
echo ""
echo "  Coding CLI turbocharger (if not already running):"
echo "  tmux new-session -d -s claude-main"
echo "  tmux send-keys -t claude-main 'claude --dangerously-skip-permissions' Enter"
echo ""
echo "  See README.md § The Turbocharger for full details."
echo ""
