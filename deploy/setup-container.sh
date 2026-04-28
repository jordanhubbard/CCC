#!/usr/bin/env bash
# setup-container.sh — Bootstrap CCC agent in a container environment
# Use this instead of setup-node.sh when running inside Kasm, Docker, or any
# container where systemd and crontab are unavailable.
#
# Usage:
#   bash deploy/setup-container.sh
#   (run from inside the cloned repo, or after cloning to ~/.ccc/workspace)
#
# What it does:
#   1. Detects that we're actually in a container (exits if not)
#   2. Symlinks ~/.ccc/workspace → this repo (if not already set up)
#   3. Creates ~/.ccc/ccc-pull-loop.sh (a while-true pull loop)
#   4. Registers ccc-pull-loop with supervisord (or falls back to nohup)
#   5. Registers ccc-exec-listener (AgentBus remote exec) with supervisord
#   6. Sets up Tailscale (userspace networking for container environments)
#   7. Starts a 'claude-main' tmux session with Claude Code
#   8. Prints a summary
#
# Idempotent: safe to run more than once.

set -euo pipefail

CCC_DIR="$HOME/.ccc"
WORKSPACE="$CCC_DIR/workspace"
PULL_LOOP="$CCC_DIR/ccc-pull-loop.sh"
EXEC_LISTENER="$CCC_DIR/ccc-exec-listener.sh"
MEMORY_SYNC="$CCC_DIR/ccc-memory-sync.sh"
LOG_DIR="$CCC_DIR/logs"
LOG_FILE="$LOG_DIR/pull.log"
EXEC_LOG="$LOG_DIR/exec-listener.log"
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
echo "🐿️  CCC Agent — Container Setup"
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

mkdir -p "$CCC_DIR"

if [ -L "$WORKSPACE" ]; then
  CURRENT_TARGET="$(readlink -f "$WORKSPACE")"
  REPO_RESOLVED="$(readlink -f "$REPO_DIR")"
  if [ "$CURRENT_TARGET" = "$REPO_RESOLVED" ]; then
    success "Workspace already symlinked: $WORKSPACE -> $REPO_DIR"
  else
    warn "Workspace symlink points to $CURRENT_TARGET, updating to $REPO_DIR"
    ln -sfn "$REPO_DIR" "$WORKSPACE"
    success "Workspace re-symlinked: $WORKSPACE -> $REPO_DIR"
  fi
elif [ -d "$WORKSPACE" ]; then
  warn "$WORKSPACE exists as a real directory, not a symlink — skipping"
else
  ln -s "$REPO_DIR" "$WORKSPACE"
  success "Workspace symlinked: $WORKSPACE -> $REPO_DIR"
fi

# ── Step 2b: Install npm dependencies ────────────────────────────────────────
# The exec-listener and other CCC tools need node_modules (better-sqlite3, etc.)
# This must happen BEFORE supervisor starts the exec-listener.
info "Installing npm dependencies..."

if command -v node &>/dev/null && command -v npm &>/dev/null; then
  # Fix .npm ownership (can get set to root inside containers)
  if [ -d "$HOME/.npm" ]; then
    chown -R "$(id -u):$(id -g)" "$HOME/.npm" 2>/dev/null || true
  fi

  # Root package.json (better-sqlite3 for exec-listener, etc.)
  RESOLVED_WORKSPACE="$(readlink -f "$WORKSPACE" 2>/dev/null || echo "$WORKSPACE")"
  if [ -f "$RESOLVED_WORKSPACE/package.json" ]; then
    (cd "$RESOLVED_WORKSPACE" && npm install --silent 2>&1) && \
      success "npm install (root): done" || \
      warn "npm install (root) failed — exec-listener may not start"
  fi

  # Dashboard deps (separate package.json)
  if [ -f "$RESOLVED_WORKSPACE/dashboard/package.json" ]; then
    (cd "$RESOLVED_WORKSPACE/dashboard" && npm install --silent 2>&1) && \
      success "npm install (dashboard): done" || \
      warn "npm install (dashboard) failed"
  fi
else
  warn "node/npm not found — skipping npm install (exec-listener will fail without it)"
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
info "Pre-creating log files..."
mkdir -p "$LOG_DIR"
touch "$LOG_FILE" "$EXEC_LOG"
chown "$USER:$USER" "$LOG_FILE" "$EXEC_LOG" 2>/dev/null || true
success "Log files: $LOG_FILE, $EXEC_LOG"

# ── Step 5a: Point exec-listener at ccc-agent listen ─────────────────────────
info "Setting up exec-listener (ccc-agent listen)..."

CCC_AGENT_BIN="$HOME/.ccc/bin/ccc-agent"
if [ ! -x "$CCC_AGENT_BIN" ]; then
  warn "ccc-agent not found at $CCC_AGENT_BIN — run migration 0011 to build it"
  warn "exec-listener will not be registered until ccc-agent is available"
  EXEC_LISTENER=""
else
  # Write a thin wrapper so supervisord has a stable path to exec
  cat > "$EXEC_LISTENER" << EOF
#!/usr/bin/env bash
# ccc-exec-listener.sh — thin wrapper for ccc-agent listen
exec $CCC_AGENT_BIN listen
EOF
  chmod +x "$EXEC_LISTENER"
  success "Exec listener wrapper: $EXEC_LISTENER → ccc-agent listen"
fi

# ── Step 5b: Supervisord integration (or nohup fallback) ─────────────────────
PULL_REGISTERED=false
EXEC_REGISTERED=false

if [ -f "$SUPERVISORD_CONF" ]; then
  info "supervisord.conf found — registering programs..."

  # Register ccc-agent-pull
  if grep -q "\[program:ccc-agent-pull\]" "$SUPERVISORD_CONF" 2>/dev/null; then
    success "ccc-agent-pull already in supervisord.conf — skipping"
    PULL_REGISTERED=true
  else
    sudo tee -a "$SUPERVISORD_CONF" > /dev/null << EOF

[program:ccc-agent-pull]
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
    success "ccc-agent-pull block appended to $SUPERVISORD_CONF"
    PULL_REGISTERED=true
  fi

  # Register ccc-exec-listener (AgentBus remote exec)
  if grep -q "\[program:ccc-exec-listener\]" "$SUPERVISORD_CONF" 2>/dev/null; then
    success "ccc-exec-listener already in supervisord.conf — skipping"
    EXEC_REGISTERED=true
  else
    sudo tee -a "$SUPERVISORD_CONF" > /dev/null << EOF

[program:ccc-exec-listener]
command=$EXEC_LISTENER
autostart=true
autorestart=true
startsecs=5
startretries=10
stdout_logfile=$EXEC_LOG
stderr_logfile=$EXEC_LOG
stdout_logfile_maxbytes=2MB
stdout_logfile_backups=2
user=$USER
EOF
    success "ccc-exec-listener block appended to $SUPERVISORD_CONF"
    EXEC_REGISTERED=true
  fi

  # Register hermes-gateway (Hermes agent runtime — Slack, Telegram, etc.)
  # bootstrap.sh writes ~/.hermes/config.yaml with the agent's tokens.
  # This block is idempotent: if bootstrap.sh already wrote a conf.d file for
  # hermes-gateway, the grep check here will still work via the main conf check.
  HERMES_BIN="$(command -v hermes 2>/dev/null || echo "$HOME/.local/bin/hermes")"
  HERMES_GW_LOG="$LOG_DIR/hermes-gateway.log"
  touch "$HERMES_GW_LOG"
  chown "$USER:$USER" "$HERMES_GW_LOG" 2>/dev/null || true

  if ! command -v hermes &>/dev/null && [ ! -x "$HOME/.local/bin/hermes" ]; then
    warn "hermes not installed — skipping hermes-gateway registration"
    warn "  Run bootstrap.sh first, then re-run setup-container.sh"
  elif grep -q "\[program:hermes-gateway\]" "$SUPERVISORD_CONF" 2>/dev/null; then
    success "hermes-gateway already in supervisord.conf — skipping"
  else
    sudo tee -a "$SUPERVISORD_CONF" > /dev/null << EOF

[program:hermes-gateway]
command=$HERMES_BIN gateway
autostart=true
autorestart=true
startsecs=10
startretries=5
stdout_logfile=$HERMES_GW_LOG
stderr_logfile=$HERMES_GW_LOG
stdout_logfile_maxbytes=5MB
stdout_logfile_backups=2
user=$USER
environment=HOME="$HOME"
directory=$HOME
priority=10
EOF
    success "hermes-gateway block appended to $SUPERVISORD_CONF"
  fi

  # Reload supervisord once for all changes
  info "Reloading supervisord..."
  sudo supervisorctl -c "$SUPERVISORD_CONF" reread && \
  sudo supervisorctl -c "$SUPERVISORD_CONF" update && \
  success "supervisord reloaded — ccc-agent-pull, ccc-exec-listener, hermes-gateway running" || \
  warn "supervisorctl update failed — you may need to restart supervisord manually"

else
  info "supervisord.conf not found — falling back to nohup background processes"

  # Pull loop
  if pgrep -f "ccc-pull-loop.sh" > /dev/null 2>&1; then
    success "ccc-pull-loop.sh already running (PID: $(pgrep -f ccc-pull-loop.sh | head -1))"
    PULL_REGISTERED=true
  else
    nohup bash "$PULL_LOOP" >> "$LOG_FILE" 2>&1 &
    PULL_PID=$!
    sleep 1
    if kill -0 "$PULL_PID" 2>/dev/null; then
      success "Pull loop started via nohup (PID: $PULL_PID)"
      echo "$PULL_PID" > "$CCC_DIR/pull-loop.pid"
    else
      warn "Pull loop may have exited immediately — check $LOG_FILE"
    fi
    PULL_REGISTERED=true
  fi

  # Exec listener
  if pgrep -f "agent-listener.mjs" > /dev/null 2>&1; then
    success "agent-listener.mjs already running (PID: $(pgrep -f agent-listener.mjs | head -1))"
    EXEC_REGISTERED=true
  else
    nohup bash "$EXEC_LISTENER" >> "$EXEC_LOG" 2>&1 &
    EXEC_PID=$!
    sleep 1
    if kill -0 "$EXEC_PID" 2>/dev/null; then
      success "Exec listener started via nohup (PID: $EXEC_PID)"
      echo "$EXEC_PID" > "$CCC_DIR/exec-listener.pid"
    else
      warn "Exec listener may have exited — check $EXEC_LOG (AGENTBUS_TOKEN required in .env)"
    fi
    EXEC_REGISTERED=true
  fi

  # Hermes gateway (nohup fallback)
  HERMES_BIN="$(command -v hermes 2>/dev/null || echo "$HOME/.local/bin/hermes")"
  HERMES_GW_LOG="$LOG_DIR/hermes-gateway.log"
  if [ ! -x "$HERMES_BIN" ]; then
    warn "hermes not installed — skipping hermes-gateway (run bootstrap.sh first)"
  elif pgrep -f "hermes gateway" > /dev/null 2>&1; then
    success "hermes gateway already running (PID: $(pgrep -f 'hermes gateway' | head -1))"
  else
    touch "$HERMES_GW_LOG"
    nohup "$HERMES_BIN" gateway >> "$HERMES_GW_LOG" 2>&1 &
    HGW_PID=$!
    sleep 2
    if kill -0 "$HGW_PID" 2>/dev/null; then
      success "hermes gateway started via nohup (PID: $HGW_PID)"
      echo "$HGW_PID" > "$CCC_DIR/hermes-gateway.pid"
    else
      warn "hermes gateway may have exited — check $HERMES_GW_LOG"
      warn "  Ensure ~/.hermes/config.yaml has SLACK_BOT_TOKEN / SLACK_APP_TOKEN"
    fi
  fi
fi

# ── Step 6: Tailscale (userspace networking — containers only) ────────────────
# Standard Tailscale requires CAP_NET_ADMIN + a kernel TUN device — both are
# unavailable in most containers.  tailscaled --tun=userspace-networking works
# anywhere because it implements the WireGuard/TUN stack entirely in userspace.
#
# Detection: IS_CONTAINER=true was established above (PID 1 is supervisord /
# docker-init / tini, or /.dockerenv / /run/.containerenv was found).  We never
# rely on knowing the host name; the same logic fires for any outbound-only
# container regardless of where it lives.
#
# Full VMs (systemd PID 1, no containerenv) use setup-node.sh instead, which
# installs Tailscale with normal kernel TUN via the standard Tailscale package.
info "Setting up Tailscale (userspace networking for container)..."

TS_DIR="$HOME/.tailscale"
TS_SOCK="$TS_DIR/tailscaled.sock"
TS_LOG="$LOG_DIR/tailscaled.log"
TS_AUTHKEY="${TS_AUTHKEY:-}"

mkdir -p "$TS_DIR"
touch "$TS_LOG" 2>/dev/null || true

# Install tailscale/tailscaled binaries if missing
if ! command -v tailscale &>/dev/null || ! command -v tailscaled &>/dev/null; then
  info "Installing Tailscale..."
  if command -v curl &>/dev/null; then
    curl -fsSL https://tailscale.com/install.sh | sh 2>/dev/null && \
      success "Tailscale installed" || \
      warn "Tailscale auto-install failed — install manually: https://tailscale.com/download/linux"
  else
    warn "curl not found — install Tailscale manually: https://tailscale.com/download/linux"
  fi
else
  success "Tailscale binaries present ($(tailscale version 2>/dev/null | head -1 || echo 'version unknown'))"
fi

# Locate supervisord drop-in directory (preferred) or fall back to main conf
SUPERVISORD_CONFD=""
for _dir in "/etc/supervisor/conf.d" "/etc/supervisord.d"; do
  if [ -d "$_dir" ]; then
    SUPERVISORD_CONFD="$_dir"
    break
  fi
done

TS_REGISTERED=false

if [ -n "$SUPERVISORD_CONFD" ]; then
  TS_CONF_FILE="$SUPERVISORD_CONFD/tailscaled.conf"
  if [ -f "$TS_CONF_FILE" ]; then
    success "tailscaled supervisord conf already exists: $TS_CONF_FILE — skipping"
    TS_REGISTERED=true
  else
    sudo tee "$TS_CONF_FILE" > /dev/null << TSEOF
[program:tailscaled]
command=tailscaled --tun=userspace-networking --socket=${TS_SOCK} --statedir=${TS_DIR}
user=${USER}
environment=HOME="${HOME}"
stdout_logfile=${TS_LOG}
stdout_logfile_maxbytes=5MB
stdout_logfile_backups=0
redirect_stderr=true
autostart=true
autorestart=true
priority=5
TSEOF
    success "tailscaled conf written: $TS_CONF_FILE"
    TS_REGISTERED=true
    sudo supervisorctl -c "$SUPERVISORD_CONF" reread 2>/dev/null && \
    sudo supervisorctl -c "$SUPERVISORD_CONF" update 2>/dev/null || \
    warn "supervisorctl reload failed — restart supervisord manually to start tailscaled"
  fi
elif [ -f "$SUPERVISORD_CONF" ]; then
  if grep -q "\[program:tailscaled\]" "$SUPERVISORD_CONF" 2>/dev/null; then
    success "tailscaled already in $SUPERVISORD_CONF — skipping"
    TS_REGISTERED=true
  else
    sudo tee -a "$SUPERVISORD_CONF" > /dev/null << TSEOF

[program:tailscaled]
command=tailscaled --tun=userspace-networking --socket=${TS_SOCK} --statedir=${TS_DIR}
user=${USER}
environment=HOME="${HOME}"
stdout_logfile=${TS_LOG}
stdout_logfile_maxbytes=5MB
stdout_logfile_backups=0
redirect_stderr=true
autostart=true
autorestart=true
priority=5
TSEOF
    success "tailscaled appended to $SUPERVISORD_CONF"
    TS_REGISTERED=true
    sudo supervisorctl -c "$SUPERVISORD_CONF" reread 2>/dev/null && \
    sudo supervisorctl -c "$SUPERVISORD_CONF" update 2>/dev/null || \
    warn "supervisorctl reload failed — restart supervisord manually"
  fi
fi

# Fallback: no supervisord config found at all — nohup
if [ "$TS_REGISTERED" = false ]; then
  warn "No supervisord config found — starting tailscaled via nohup"
  if pgrep -f "tailscaled" > /dev/null 2>&1; then
    success "tailscaled already running"
  else
    nohup tailscaled --tun=userspace-networking --socket="$TS_SOCK" --statedir="$TS_DIR" \
      >> "$TS_LOG" 2>&1 &
    sleep 2
    success "tailscaled started via nohup"
  fi
fi

# Wait up to 10s for tailscaled to become responsive, then run tailscale up
_ts_wait=0
while [ "$_ts_wait" -lt 10 ]; do
  tailscale --socket="$TS_SOCK" status &>/dev/null 2>&1 && break
  sleep 1
  _ts_wait=$((_ts_wait + 1))
done

if tailscale --socket="$TS_SOCK" status &>/dev/null 2>&1; then
  TS_BACKEND=$(tailscale --socket="$TS_SOCK" status --json 2>/dev/null \
    | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('BackendState',''))" \
    2>/dev/null || echo "")
  if [ "$TS_BACKEND" = "Running" ]; then
    TS_IP=$(tailscale --socket="$TS_SOCK" ip -4 2>/dev/null || echo "unknown")
    success "Tailscale connected (IP: $TS_IP)"
  else
    info "Running tailscale up..."
    TS_UP_CMD="tailscale --socket=${TS_SOCK} up"
    [ -n "$TS_AUTHKEY" ] && TS_UP_CMD="$TS_UP_CMD --authkey=${TS_AUTHKEY}"
    $TS_UP_CMD 2>&1 | tee -a "$TS_LOG" || \
      warn "tailscale up incomplete — if an auth URL was printed, visit it to authorize"
    warn "  Or run manually: tailscale --socket=${TS_SOCK} up"
    [ -z "$TS_AUTHKEY" ] && \
      warn "  For unattended setup, set TS_AUTHKEY in ~/.ccc/.env and re-run this script"
  fi
else
  warn "tailscaled not yet responding — supervisord will start it on next boot"
  warn "  Then run: tailscale --socket=${TS_SOCK} up"
fi

# ── Step 7: Claude tmux session ─────────────────────────────────────────────
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
echo "  Workspace:      $WORKSPACE"
echo "  Pull loop:      $PULL_LOOP"
echo "  Exec listener:  $EXEC_LISTENER"
echo "  Tailscale:      $TS_DIR"
echo "  Pull log:       $LOG_FILE"
echo "  Exec log:       $EXEC_LOG"
echo "  Tailscale log:  $TS_LOG"
echo ""

if [ -f "$SUPERVISORD_CONF" ] || [ -n "$SUPERVISORD_CONFD" ]; then
  echo "  Process manager: supervisord"
  echo "  Programs:        ccc-agent-pull, ccc-exec-listener, hermes-gateway, tailscaled"
  echo "  Check:           sudo supervisorctl -c $SUPERVISORD_CONF status"
else
  echo "  Process manager: nohup background"
  echo "  Check pull:      pgrep -fa ccc-pull-loop"
  echo "  Check exec:      pgrep -fa agent-listener"
  echo "  Check tailscale: pgrep -fa tailscaled"
  echo "  PID files:       $CCC_DIR/pull-loop.pid, $CCC_DIR/exec-listener.pid"
fi

echo ""
echo "  Verify these manually:"
echo "  1. ~/.ccc/.env exists with AGENT_NAME, CCC_URL, CCC_AGENT_TOKEN, AGENTBUS_TOKEN"
echo "  2. Pull loop running:      tail -f $LOG_FILE"
echo "  3. Exec listener running:  tail -f $EXEC_LOG"
echo "  4. Tailscale status:       tailscale --socket=${TS_SOCK} status"
echo "  5. Claude session:         tmux attach -t claude-main"
echo ""
echo "  If .env is missing:"
echo "    cp $REPO_DIR/deploy/.env.template ~/.ccc/.env"
echo "    nano ~/.ccc/.env"
echo ""
echo "  Required .env keys for exec listener:"
echo "    AGENTBUS_TOKEN       — shared bus secret (get from Rocky/CCC admin)"
echo "    AGENTBUS_URL         — http://<CCC_HOST>:8789"
echo "    CCC_URL             — http://<CCC_HOST>:8789"
echo "    CCC_AGENT_TOKEN     — your agent bearer token"
echo "    AGENT_NAME          — your agent name (peabody, sherman, etc.)"
echo ""
echo "  Optional .env key for Tailscale:"
echo "    TS_AUTHKEY          — pre-auth key for unattended join (generate in Tailscale admin console)"
echo ""
