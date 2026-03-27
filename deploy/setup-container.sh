#!/usr/bin/env bash
# setup-container.sh — Bootstrap RCC agent in a container environment
# Use this instead of setup-node.sh when running inside Kasm, Docker, or any
# container where systemd and crontab are unavailable.
#
# Usage:
#   bash deploy/setup-container.sh
#   (run from inside the cloned repo, or after cloning to ~/.rcc/workspace)
#
# What it does:
#   1. Detects that we're actually in a container (exits if not)
#   2. Symlinks ~/.rcc/workspace → this repo (if not already set up)
#   3. Creates ~/.rcc/rcc-pull-loop.sh (a while-true pull loop)
#   4. Registers rcc-pull-loop with supervisord (or falls back to nohup)
#   5. Starts a 'claude-main' tmux session with Claude Code
#   6. Pre-creates the log file
#   7. Prints a summary
#
# Idempotent: safe to run more than once.

set -euo pipefail

RCC_DIR="$HOME/.rcc"
WORKSPACE="$RCC_DIR/workspace"
PULL_LOOP="$RCC_DIR/rcc-pull-loop.sh"
LOG_DIR="$RCC_DIR/logs"
LOG_FILE="$LOG_DIR/pull.log"
SUPERVISORD_CONF="/etc/supervisord.conf"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── Colors ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()    { echo -e "${BLUE}[container-setup]${NC} $1"; }
success() { echo -e "${GREEN}[container-setup]${NC} ✓ $1"; }
warn()    { echo -e "${YELLOW}[container-setup]${NC} ⚠ $1"; }
error()   { echo -e "${RED}[container-setup]${NC} ✗ $1" >&2; exit 1; }

echo ""
echo "🐿️  RCC Agent — Container Setup"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# ── Step 1: Container detection ─────────────────────────────────────────────
# PID 1 in a container is typically supervisord, docker-init, tini, or a shell.
# On a real host with systemd it will be /sbin/init or /lib/systemd/systemd.
info "Detecting environment..."

PID1_COMM=""
if [ -r /proc/1/comm ]; then
  PID1_COMM="$(cat /proc/1/comm)"
fi

PID1_EXE=""
if [ -L /proc/1/exe ]; then
  PID1_EXE="$(readlink -f /proc/1/exe 2>/dev/null || true)"
fi

IS_CONTAINER=false

# Check for explicit container markers
if [ -f /.dockerenv ] || [ -f /run/.containerenv ]; then
  IS_CONTAINER=true
  info "Container marker found (/.dockerenv or /run/.containerenv)"
fi

# Check PID 1 — if it's not systemd or init it's almost certainly a container
case "$PID1_COMM" in
  supervisord|supervisord-*|docker-init|tini|dumb-init|s6-svscan|runit|openrc-init|bash|sh)
    IS_CONTAINER=true
    info "PID 1 is '$PID1_COMM' — container confirmed"
    ;;
  systemd|init)
    # Could be a systemd container, but probably a real host
    if [ "$IS_CONTAINER" = false ]; then
      info "PID 1 is '$PID1_COMM' — looks like a regular host"
    fi
    ;;
  *)
    info "PID 1 comm: '${PID1_COMM:-unknown}' (exe: ${PID1_EXE:-unknown})"
    ;;
esac

# Final verdict
if [ "$IS_CONTAINER" = false ]; then
  echo ""
  echo "  This does not appear to be a container environment."
  echo "  PID 1: ${PID1_COMM:-unknown} (${PID1_EXE:-unknown})"
  echo ""
  echo "  For regular Linux hosts, use setup-node.sh instead:"
  echo "    bash $REPO_DIR/deploy/setup-node.sh"
  echo ""
  echo "  To force container setup anyway, re-run with:"
  echo "    FORCE_CONTAINER=1 bash deploy/setup-container.sh"
  echo ""
  if [ "${FORCE_CONTAINER:-0}" != "1" ]; then
    exit 0
  fi
  warn "FORCE_CONTAINER=1 set — proceeding anyway"
fi

success "Environment: container"

# ── Step 2: Symlink workspace ────────────────────────────────────────────────
info "Checking workspace symlink..."

mkdir -p "$RCC_DIR"

if [ -L "$WORKSPACE" ]; then
  CURRENT_TARGET="$(readlink -f "$WORKSPACE")"
  if [ "$CURRENT_TARGET" = "$REPO_DIR" ]; then
    success "Workspace already symlinked: $WORKSPACE -> $REPO_DIR"
  else
    warn "Workspace symlink exists but points elsewhere: $WORKSPACE -> $CURRENT_TARGET"
    warn "Expected: $REPO_DIR"
    warn "Skipping — update manually if needed: ln -sfn $REPO_DIR $WORKSPACE"
  fi
elif [ -d "$WORKSPACE" ]; then
  warn "$WORKSPACE exists as a real directory, not a symlink"
  warn "Skipping symlink creation. If this is wrong, remove it and re-run."
else
  ln -s "$REPO_DIR" "$WORKSPACE"
  success "Workspace symlinked: $WORKSPACE -> $REPO_DIR"
fi

# ── Step 3: Create pull loop script ─────────────────────────────────────────
info "Creating pull loop script..."

cat > "$PULL_LOOP" << 'EOF'
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
while true; do
  bash "$SCRIPT_DIR/workspace/deploy/agent-pull.sh" || true
  sleep 600
done
EOF

chmod +x "$PULL_LOOP"
success "Pull loop script: $PULL_LOOP"

# ── Step 4: Log file pre-creation ───────────────────────────────────────────
info "Pre-creating log file..."
mkdir -p "$LOG_DIR"
touch "$LOG_FILE"
chown "$USER:$USER" "$LOG_FILE" 2>/dev/null || true
success "Log file: $LOG_FILE"

# ── Step 5: Supervisord integration (or nohup fallback) ─────────────────────
PULL_REGISTERED=false

if [ -f "$SUPERVISORD_CONF" ]; then
  info "supervisord.conf found — registering rcc-agent-pull program..."

  # Check if already registered
  if grep -q "\[program:rcc-agent-pull\]" "$SUPERVISORD_CONF" 2>/dev/null; then
    success "rcc-agent-pull already in supervisord.conf — skipping"
    PULL_REGISTERED=true
  else
    # Append the program block
    sudo tee -a "$SUPERVISORD_CONF" > /dev/null << EOF

[program:rcc-agent-pull]
command=$PULL_LOOP
autostart=true
autorestart=true
startsecs=5
startretries=3
stdout_logfile=$LOG_FILE
stderr_logfile=$LOG_FILE
stdout_logfile_maxbytes=1MB
stdout_logfile_backups=1
user=$USER
EOF
    success "rcc-agent-pull block appended to $SUPERVISORD_CONF"

    # Reload supervisord
    info "Reloading supervisord..."
    sudo supervisorctl -c "$SUPERVISORD_CONF" reread && \
    sudo supervisorctl -c "$SUPERVISORD_CONF" update && \
    success "supervisord reloaded — rcc-agent-pull is running" || \
    warn "supervisorctl update failed — you may need to restart supervisord manually"

    PULL_REGISTERED=true
  fi
else
  info "supervisord.conf not found — falling back to nohup background process"

  # Check if pull loop is already running
  if pgrep -f "rcc-pull-loop.sh" > /dev/null 2>&1; then
    success "rcc-pull-loop.sh already running (PID: $(pgrep -f rcc-pull-loop.sh | head -1))"
    PULL_REGISTERED=true
  else
    nohup bash "$PULL_LOOP" >> "$LOG_FILE" 2>&1 &
    PULL_PID=$!
    sleep 1
    if kill -0 "$PULL_PID" 2>/dev/null; then
      success "Pull loop started via nohup (PID: $PULL_PID)"
      echo "$PULL_PID" > "$RCC_DIR/pull-loop.pid"
    else
      warn "Pull loop may have exited immediately — check $LOG_FILE"
    fi
    PULL_REGISTERED=true
  fi
fi

# ── Step 6: Claude tmux session ─────────────────────────────────────────────
info "Checking claude-main tmux session..."

if ! command -v tmux &>/dev/null; then
  warn "tmux not found — skipping claude-main session"
  warn "Install with: sudo apt-get install -y tmux"
else
  if tmux has-session -t claude-main 2>/dev/null; then
    success "tmux session 'claude-main' already exists — skipping"
  else
    info "Creating tmux session 'claude-main'..."
    tmux new-session -d -s claude-main

    sleep 1

    # Start Claude Code with permissions bypass
    tmux send-keys -t claude-main "claude --dangerously-skip-permissions" ""

    sleep 1

    # NOTE: The Down arrow must be sent as a separate tmux send-keys call.
    # Combining it with the command string doesn't work — tmux only processes
    # key names (like "Down") when they are the sole argument to send-keys.
    tmux send-keys -t claude-main "Down" ""

    sleep 0.5

    tmux send-keys -t claude-main "" Enter

    success "claude-main tmux session started"
    echo ""
    echo "  Note: The Down arrow key is sent as a separate tmux call."
    echo "  This is intentional — tmux requires key names to be sent alone."
    echo "  If Claude Code doesn't start correctly, run:"
    echo "    tmux attach -t claude-main"
  fi
fi

# ── Summary ──────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "🐿️  ${GREEN}Container setup complete!${NC}"
echo ""
echo "  Workspace:   $WORKSPACE"
echo "  Pull loop:   $PULL_LOOP"
echo "  Logs:        $LOG_FILE"
echo ""

if [ -f "$SUPERVISORD_CONF" ]; then
  echo "  Pull method: supervisord (program: rcc-agent-pull)"
  echo "  Check:       sudo supervisorctl -c $SUPERVISORD_CONF status"
else
  echo "  Pull method: nohup background process"
  echo "  Check:       pgrep -fa rcc-pull-loop"
  echo "  PID file:    $RCC_DIR/pull-loop.pid"
fi

echo ""
echo "  Verify these manually:"
echo "  1. ~/.rcc/.env exists with AGENT_NAME, RCC_URL, RCC_AGENT_TOKEN"
echo "  2. Pull loop is running: tail -f $LOG_FILE"
echo "  3. Claude session: tmux attach -t claude-main"
echo ""
echo "  If .env is missing:"
echo "    cp $REPO_DIR/deploy/.env.template ~/.rcc/.env"
echo "    nano ~/.rcc/.env"
echo ""
