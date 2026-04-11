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

# Install coding-agent skill via clawhub (if openclaw + clawhub present)
if command -v openclaw &>/dev/null && command -v clawhub &>/dev/null; then
  if ! openclaw skills list 2>/dev/null | grep -q "coding-agent"; then
    info "Installing coding-agent skill..."
    clawhub install coding-agent 2>/dev/null && \
    success "coding-agent skill installed" || \
    warn "Could not install coding-agent skill. Run: clawhub install coding-agent"
  else
    success "coding-agent skill already installed"
  fi
elif command -v openclaw &>/dev/null; then
  warn "clawhub not found. Install it to get the coding-agent skill: npm install -g clawhub"
fi

# ── Install agent runtime (Hermes preferred; OpenClaw fallback) ──────────
info "Checking agent runtime..."

HERMES_INSTALLED=false
OPENCLAW_INSTALLED=false

if command -v hermes &>/dev/null; then
  HERMES_INSTALLED=true
  success "Hermes agent present ($(hermes --version 2>/dev/null | head -1))"
elif command -v openclaw &>/dev/null; then
  OPENCLAW_INSTALLED=true
  success "OpenClaw present — Hermes not found, using OpenClaw"
else
  info "No agent runtime found — installing Hermes..."
  HERMES_INSTALL_OK=false
  if command -v pipx &>/dev/null; then
    # pipx is the cleanest path everywhere (avoids PEP 668 on modern macOS/Linux)
    pipx install hermes-agent --quiet 2>/dev/null && HERMES_INSTALL_OK=true || true
  elif command -v pip3 &>/dev/null || command -v pip &>/dev/null; then
    PIP="$(command -v pip3 || command -v pip)"
    if [[ "$PLATFORM" == "macos" ]]; then
      # PEP 668: Homebrew Python refuses bare pip install on macOS 3.11+
      # Try pipx first (above), then fall back to a venv install
      HERMES_VENV="$HOME/.hermes-install-venv"
      python3 -m venv "$HERMES_VENV" --quiet 2>/dev/null && \
        "$HERMES_VENV/bin/pip" install --quiet hermes-agent 2>/dev/null && \
        ln -sf "$HERMES_VENV/bin/hermes" "$HOME/.local/bin/hermes" 2>/dev/null && \
        HERMES_INSTALL_OK=true || true
    else
      # Linux: bare pip3 is fine
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
    echo "  │  Install an agent runtime manually:                          │"
    echo "  │                                                              │"
    echo "  │  Hermes (recommended):                                       │"
    echo "  │    macOS:  pipx install hermes-agent   (preferred)           │"
    echo "  │            brew install pipx && pipx install hermes-agent    │"
    echo "  │    Linux:  pip3 install hermes-agent                         │"
    echo "  │                                                              │"
    echo "  │  OpenClaw (Node.js alternative):                             │"
    echo "  │    npm install -g openclaw                                   │"
    echo "  │                                                              │"
    echo "  │  Then re-run this script to complete setup.                  │"
    echo "  └──────────────────────────────────────────────────────────────┘"
    echo ""
  fi
fi

# Migrate existing OpenClaw config into Hermes (if applicable)
if [ "$HERMES_INSTALLED" = true ] && command -v hermes &>/dev/null; then
  if [ -f "$HOME/.openclaw/config.json" ] || [ -d "$HOME/.openclaw" ]; then
    if ! [ -f "$HOME/.hermes/config.json" ]; then
      info "Found existing OpenClaw config — running hermes claw migrate..."
      hermes claw migrate 2>/dev/null && \
        success "OpenClaw config migrated to Hermes" || \
        warn "Migration had warnings — check ~/.hermes/config.json"
    else
      success "Hermes config already exists — skipping migration"
    fi
  fi
fi

# Install ccc-node skill into whichever runtime is active
CCC_SKILL_SRC="$WORKSPACE/skills/ccc-node"
if [ -d "$CCC_SKILL_SRC" ]; then
  if [ "$HERMES_INSTALLED" = true ] && command -v hermes &>/dev/null; then
    SKILL_DEST="$HOME/.hermes/skills/ccc-node"
    if [ ! -d "$SKILL_DEST" ]; then
      cp -r "$CCC_SKILL_SRC" "$SKILL_DEST"
      success "ccc-node skill installed into Hermes"
    else
      success "ccc-node skill already in Hermes"
    fi
  elif [ "$OPENCLAW_INSTALLED" = true ] && command -v openclaw &>/dev/null; then
    SKILL_DEST="$HOME/.local/lib/node_modules/openclaw/skills/ccc-node"
    if [ ! -d "$SKILL_DEST" ]; then
      cp -r "$CCC_SKILL_SRC" "$SKILL_DEST"
      success "ccc-node skill installed into OpenClaw"
    else
      success "ccc-node skill already in OpenClaw"
    fi
  fi
fi

# ── ClawFS / JuiceFS (shared model/file cache) ───────────────────────────
info "Checking ClawFS (JuiceFS FUSE mount)..."
CLAWFS_MOUNT="${CLAWFS_MOUNT:-$HOME/clawfs}"
CLAWFS_REDIS="redis://100.89.199.14:6379/1"
CLAWFS_CACHE="/tmp/jfscache"

_clawfs_mounted() { [[ -f "${CLAWFS_MOUNT}/.config" ]]; }

if command -v juicefs &>/dev/null; then
  success "JuiceFS installed ($(juicefs --version 2>/dev/null | head -1))"
  if _clawfs_mounted; then
    success "ClawFS already mounted at $CLAWFS_MOUNT"
  else
    warn "JuiceFS installed but ClawFS not mounted — bootstrap.sh will mount it"
  fi
elif [[ "$PLATFORM" == "linux" ]]; then
  warn "JuiceFS not found — bootstrap.sh will install it automatically"
  warn "  Or install manually: curl -sSL https://juicefs.com/static/juicefs -o /usr/local/bin/juicefs && chmod +x /usr/local/bin/juicefs"
else
  warn "JuiceFS not found — to enable ClawFS on macOS:"
  warn "  1. brew install --cask macfuse   (reboot + approve system extension)"
  warn "  2. brew install juicefs"
  warn "  3. juicefs mount --background --cache-dir /tmp/jfscache redis://100.89.199.14:6379/1 ~/clawfs"
fi

# Check FUSE availability (Linux)
if [[ "$PLATFORM" == "linux" ]]; then
  if command -v fusermount &>/dev/null || command -v fusermount3 &>/dev/null; then
    success "FUSE utils present"
  else
    warn "FUSE utils not found — install: sudo apt-get install -y fuse3"
  fi
fi

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
      sudo cp "$SERVICE_SRC" "$SERVICE_DST" 2>/dev/null && \
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
  NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  if command -v node >/dev/null 2>&1; then
    node -e "
      require('fs').writeFileSync('$CCC_DIR/agent.json', JSON.stringify({
        schema_version: 1,
        agent_name: '${AGENT_NAME:-unknown}',
        host: '$(hostname)',
        onboarded_at: '$NOW',
        onboarded_by: 'setup-node.sh',
        ccc_version: '$CCC_VERSION',
        last_upgraded_at: '$NOW',
        last_upgraded_version: '$CCC_VERSION'
      }, null, 2) + '\n');
    " && chmod 600 "$CCC_DIR/agent.json" && success "Onboarding signature written to $CCC_DIR/agent.json" \
      || warn "Failed to write agent.json (non-fatal)"
  else
    warn "node not found — skipping agent.json write"
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
echo "  4. Start agent runtime: hermes gateway   (or: openclaw start)"
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
