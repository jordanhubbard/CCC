#!/bin/bash
# setup-node.sh — Bootstrap a new RCC agent node
# Run once on a new machine. Safe to re-run (idempotent).
#
# Usage:
#   REPO_URL=git@github.com:yourorg/your-rcc-repo.git bash deploy/setup-node.sh
#   OR (from inside an existing clone): bash deploy/setup-node.sh
#
# Tip: run deploy/rcc-init.sh after this to configure your .env interactively.

set -e

REPO_URL="${REPO_URL:-${REPO_URL:-git@github.com:<your-org>/rockyandfriends.git}}"
RCC_DIR="$HOME/.rcc"
WORKSPACE="$RCC_DIR/workspace"
ENV_FILE="$RCC_DIR/.env"
LOG_DIR="$RCC_DIR/logs"
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
echo "🐿️  RCC Agent Node Setup"
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
mkdir -p "$RCC_DIR" "$LOG_DIR"
success "Created $RCC_DIR"

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
  echo "  │  Required: AGENT_NAME, AGENT_HOST, RCC_URL"
  echo "  │            RCC_AGENT_TOKEN, NVIDIA_API_KEY"
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
  PLIST_SRC="$WORKSPACE/deploy/launchd/com.rcc.agent.plist"
  PLIST_DST="$HOME/Library/LaunchAgents/com.rcc.agent.plist"
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

# ── Set up systemd service (Linux only) ───────────────────────────────────
if [[ "$PLATFORM" == "linux" ]] && command -v systemctl &>/dev/null; then
  SERVICE_SRC="$WORKSPACE/deploy/systemd/rcc-agent.service"
  SERVICE_DST="/etc/systemd/system/rcc-agent.service"
  if [ -f "$SERVICE_DST" ]; then
    warn "rcc-agent.service already installed — skipping"
  else
    if [ -w "/etc/systemd/system" ] || command -v sudo &>/dev/null; then
      sudo cp "$SERVICE_SRC" "$SERVICE_DST" 2>/dev/null && \
      sudo systemctl daemon-reload && \
      sudo systemctl enable rcc-agent && \
      sudo systemctl start rcc-agent && \
      success "rcc-agent systemd service installed and started" || \
      warn "Could not install systemd service (needs sudo). Run manually: sudo systemctl enable --now rcc-agent"
    else
      warn "No sudo access — install systemd service manually"
    fi
  fi
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
echo ""
echo "  To register this agent with RCC:"
echo "  bash $WORKSPACE/deploy/register-agent.sh"
echo ""
echo "  Coding CLI turbocharger (if not already running):"
echo "  tmux new-session -d -s claude-main"
echo "  tmux send-keys -t claude-main 'claude --dangerously-skip-permissions' Enter"
echo ""
echo "  See README.md § The Turbocharger for full details."
echo ""
