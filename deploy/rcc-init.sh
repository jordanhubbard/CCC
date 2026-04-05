#!/bin/bash
# rcc-init.sh — Interactive CCC onboarding
# Prompts for agent identity and role, then writes a filled-in ~/.rcc/.env
# Safe to re-run: backs up any existing .env before overwriting.
#
# Usage: bash deploy/rcc-init.sh

set -e

RCC_DIR="$HOME/.rcc"
WORKSPACE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="$RCC_DIR/.env"
TEMPLATE="$WORKSPACE_DIR/deploy/.env.template"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()    { echo -e "${BLUE}[rcc-init]${NC} $1"; }
success() { echo -e "${GREEN}[rcc-init]${NC} ✓ $1"; }
warn()    { echo -e "${YELLOW}[rcc-init]${NC} ⚠ $1"; }
error()   { echo -e "${RED}[rcc-init]${NC} ✗ $1"; exit 1; }
ask()     { echo -e "${CYAN}${BOLD}?${NC} $1"; }

# ── Prompt helper ──────────────────────────────────────────────────────────
# prompt VAR_NAME "Question" "default"
prompt() {
  local var="$1" question="$2" default="$3"
  if [ -n "$default" ]; then
    ask "$question [${default}]: "
  else
    ask "$question: "
  fi
  read -r value
  if [ -z "$value" ] && [ -n "$default" ]; then
    value="$default"
  fi
  eval "$var=\"\$value\""
}

# ── Banner ─────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e " ${BOLD}CCC Agent Onboarding${NC}  (rcc-init.sh)"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "This script configures a new CCC agent node."
echo "It will write ~/.rcc/.env from your answers."
echo ""

# ── Verify template exists ─────────────────────────────────────────────────
if [ ! -f "$TEMPLATE" ]; then
  error "Template not found at $TEMPLATE — run from repo root or set WORKSPACE_DIR"
fi

mkdir -p "$RCC_DIR"

# ── Backup existing .env ───────────────────────────────────────────────────
if [ -f "$ENV_FILE" ]; then
  BACKUP="${ENV_FILE}.bak.$(date +%Y%m%d%H%M%S)"
  cp "$ENV_FILE" "$BACKUP"
  warn "Existing .env backed up to $BACKUP"
fi

# ═══════════════════════════════════════════════════════════════════════════
# STEP 1 — Agent Identity
# ═══════════════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}Step 1: Agent Identity${NC}"
echo ""

prompt AGENT_NAME "Short unique name for this agent (e.g. builder, gpu-box, myagent)" ""
while [ -z "$AGENT_NAME" ]; do
  warn "AGENT_NAME cannot be empty."
  prompt AGENT_NAME "Short unique name for this agent" ""
done

prompt AGENT_HOST "Human-readable hostname for dashboard display" "$(hostname)"

# ═══════════════════════════════════════════════════════════════════════════
# STEP 2 — CCC Role
# ═══════════════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}Step 2: CCC Role${NC}"
echo ""
echo "Is this node the CCC host (runs the central API server),"
echo "or a client node that connects to an existing CCC host?"
echo ""
echo "  1) CCC host  — this machine will run the API on port 8789"
echo "  2) Client    — this machine connects to a remote CCC host"
echo ""
ask "Enter 1 or 2 [2]: "
read -r ROLE_CHOICE
ROLE_CHOICE="${ROLE_CHOICE:-2}"

IS_RCC_HOST=false
if [ "$ROLE_CHOICE" = "1" ]; then
  IS_RCC_HOST=true
fi

# ═══════════════════════════════════════════════════════════════════════════
# STEP 3 — CCC Connection
# ═══════════════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}Step 3: CCC Connection${NC}"
echo ""

if $IS_RCC_HOST; then
  info "This node IS the CCC host."
  prompt RCC_PORT "CCC API port" "8789"
  CCC_URL="http://localhost:${RCC_PORT}"
  echo ""
  echo "  You will need to set CCC_AUTH_TOKENS to a comma-separated list"
  echo "  of bearer tokens. Generate them with: openssl rand -hex 32"
  echo ""
  prompt CCC_AUTH_TOKENS "CCC auth tokens (comma-separated, or press enter to set later)" ""
  RCC_HOST_PUBLIC=""
  prompt RCC_HOST_PUBLIC "Public hostname or IP for other agents to reach this host (optional)" ""
else
  info "This node is a CLIENT — connecting to an existing CCC host."
  echo ""
  prompt RCC_HOST_INPUT "CCC host URL (e.g. https://rcc.example.com or http://10.0.0.1:8789)" ""
  while [ -z "$RCC_HOST_INPUT" ]; do
    warn "CCC URL cannot be empty for a client node."
    prompt RCC_HOST_INPUT "CCC host URL" ""
  done
  CCC_URL="$RCC_HOST_INPUT"
  RCC_PORT=""
  CCC_AUTH_TOKENS=""
  RCC_HOST_PUBLIC=""
fi

echo ""
prompt CCC_AGENT_TOKEN "Bearer token for this agent (issued by CCC admin, or leave blank to set later)" ""

# ═══════════════════════════════════════════════════════════════════════════
# STEP 4 — Capabilities
# ═══════════════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}Step 4: Agent Capabilities${NC}"
echo ""

ask "Does this node have a Claude Code CLI tmux session? (true/false) [false]: "
read -r AGENT_CLAUDE_CLI
AGENT_CLAUDE_CLI="${AGENT_CLAUDE_CLI:-false}"

if [ "$AGENT_CLAUDE_CLI" = "true" ]; then
  prompt AGENT_CLAUDE_MODEL "Claude model" "claude-sonnet-4-6"
else
  AGENT_CLAUDE_MODEL="claude-sonnet-4-6"
fi

ask "Does this node have GPU(s)? (true/false) [false]: "
read -r AGENT_HAS_GPU
AGENT_HAS_GPU="${AGENT_HAS_GPU:-false}"

AGENT_GPU_MODEL=""
AGENT_GPU_COUNT="0"
AGENT_GPU_VRAM_GB="0"
if [ "$AGENT_HAS_GPU" = "true" ]; then
  prompt AGENT_GPU_MODEL "GPU model string (e.g. RTX 4090, H100, L40)" ""
  prompt AGENT_GPU_COUNT "Number of GPUs" "1"
  prompt AGENT_GPU_VRAM_GB "Total GPU VRAM in GB" ""
fi

# ═══════════════════════════════════════════════════════════════════════════
# STEP 5 — Communication Channels
# ═══════════════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}Step 5: Communication Channels${NC}"
echo ""
echo "  Select which channels to enable (comma-separated, or press enter for SquirrelChat only):"
echo ""
echo "    squirrelchat  — Self-hosted chat (ships with CCC, always available)"
echo "    slack          — Slack workspace integration"
echo "    mattermost     — Mattermost server integration"
echo "    telegram       — Telegram bot integration"
echo ""
echo "  Examples:  slack,telegram    or    slack    or    (blank for SquirrelChat only)"
echo ""
ask "Channels [squirrelchat]: "
read -r CHANNEL_SELECTION
CHANNEL_SELECTION="${CHANNEL_SELECTION:-squirrelchat}"

# Parse channel selection
SLACK_TOKEN=""
SLACK_SIGNING_SECRET=""
MATTERMOST_TOKEN=""
MATTERMOST_URL=""
TELEGRAM_TOKEN=""

if echo "$CHANNEL_SELECTION" | grep -qi "slack"; then
  echo ""
  info "Configuring Slack..."
  prompt SLACK_TOKEN "Slack bot token (xoxb-...)" ""
  prompt SLACK_SIGNING_SECRET "Slack signing secret (from app settings → Basic Information)" ""
fi

if echo "$CHANNEL_SELECTION" | grep -qi "mattermost"; then
  echo ""
  info "Configuring Mattermost..."
  prompt MATTERMOST_URL "Mattermost server URL (e.g. https://chat.example.com)" ""
  prompt MATTERMOST_TOKEN "Mattermost bot/personal access token" ""
fi

if echo "$CHANNEL_SELECTION" | grep -qi "telegram"; then
  echo ""
  info "Configuring Telegram..."
  prompt TELEGRAM_TOKEN "Telegram bot token (from @BotFather)" ""
fi

if [ "$CHANNEL_SELECTION" = "squirrelchat" ] || [ -z "$CHANNEL_SELECTION" ]; then
  echo ""
  success "SquirrelChat will be your default communication channel."
  echo "  It starts automatically with the CCC stack — no external accounts needed."
fi

# ═══════════════════════════════════════════════════════════════════════════
# STEP 6 — Optional Infrastructure
# ═══════════════════════════════════════════════════════════════════════════
echo ""
echo -e "${BOLD}Step 6: Optional Infrastructure${NC} (press enter to skip)"
echo ""

prompt NVIDIA_API_KEY "NVIDIA API key (for LLM inference)" ""
prompt MINIO_ENDPOINT "MinIO endpoint (e.g. http://10.0.0.5:9000)" ""
prompt MINIO_ACCESS_KEY "MinIO access key" ""
prompt MINIO_SECRET_KEY "MinIO secret key" ""
MINIO_BUCKET="agents"

# ═══════════════════════════════════════════════════════════════════════════
# WRITE .env
# ═══════════════════════════════════════════════════════════════════════════
echo ""
info "Writing $ENV_FILE ..."

cat > "$ENV_FILE" << EOF
# CCC Agent Node — Environment Configuration
# Generated by rcc-init.sh on $(date -u '+%Y-%m-%dT%H:%M:%SZ')
# NEVER commit this file to git.

# ── CCC Connection ─────────────────────────────────────────────────────────
CCC_URL=${CCC_URL}
CCC_AGENT_TOKEN=${CCC_AGENT_TOKEN}

# ── Agent Identity ─────────────────────────────────────────────────────────
AGENT_NAME=${AGENT_NAME}
AGENT_HOST=${AGENT_HOST}

# ── Agent Capabilities ─────────────────────────────────────────────────────
AGENT_CLAUDE_CLI=${AGENT_CLAUDE_CLI}
AGENT_CLAUDE_MODEL=${AGENT_CLAUDE_MODEL}
AGENT_HAS_GPU=${AGENT_HAS_GPU}
AGENT_GPU_MODEL=${AGENT_GPU_MODEL}
AGENT_GPU_COUNT=${AGENT_GPU_COUNT}
AGENT_GPU_VRAM_GB=${AGENT_GPU_VRAM_GB}

# ── NVIDIA Inference ───────────────────────────────────────────────────────
NVIDIA_API_BASE=https://inference-api.nvidia.com/v1
NVIDIA_API_KEY=${NVIDIA_API_KEY}
# TokenHub — preferred inference router (aggregates local vLLM + NVIDIA NIM)
TOKENHUB_URL=http://146.190.134.110:8090
TOKENHUB_AGENT_KEY=${TOKENHUB_AGENT_KEY:-}

# ── Storage: MinIO ─────────────────────────────────────────────────────────
MINIO_ENDPOINT=${MINIO_ENDPOINT}
MINIO_ACCESS_KEY=${MINIO_ACCESS_KEY}
MINIO_SECRET_KEY=${MINIO_SECRET_KEY}
MINIO_BUCKET=${MINIO_BUCKET}

# ── Storage: Azure Blob ────────────────────────────────────────────────────
AZURE_BLOB_PUBLIC_URL=
AZURE_BLOB_SAS_TOKEN=

# ── Channel Integrations ───────────────────────────────────────────────────
# Channels selected during init: ${CHANNEL_SELECTION}
SLACK_TOKEN=${SLACK_TOKEN}
SLACK_SIGNING_SECRET=${SLACK_SIGNING_SECRET}
MATTERMOST_URL=${MATTERMOST_URL}
MATTERMOST_TOKEN=${MATTERMOST_TOKEN}
TELEGRAM_TOKEN=${TELEGRAM_TOKEN}
EOF

# Append CCC host config if this is the hub
if $IS_RCC_HOST; then
  cat >> "$ENV_FILE" << EOF

# ── CCC API Server (this node IS the hub) ──────────────────────────────────
RCC_PORT=${RCC_PORT}
CCC_AUTH_TOKENS=${CCC_AUTH_TOKENS}
WQ_API_TOKEN=
DEFAULT_TRIAGING_AGENT=${AGENT_NAME}
EOF
  if [ -n "$RCC_HOST_PUBLIC" ]; then
    echo "RCC_HOST_PUBLIC=${RCC_HOST_PUBLIC}" >> "$ENV_FILE"
  fi
fi

chmod 600 "$ENV_FILE"
success ".env written (chmod 600)"

# ═══════════════════════════════════════════════════════════════════════════
# TEMPLATE RENDERING: substitute {{RCC_HOST}} / {{GITHUB_USER}} in *.tmpl files
# ═══════════════════════════════════════════════════════════════════════════
if $IS_RCC_HOST && [ -n "$RCC_HOST_PUBLIC" ]; then
  echo ""
  info "Rendering deployment templates (*.tmpl → live configs)..."
  GITHUB_USER_VAL=""
  prompt GITHUB_USER_VAL "GitHub username (for project URLs, or leave blank to skip)" ""
  find "$WORKSPACE_DIR" -name "*.tmpl" \
    ! -path "*/.git/*" \
    ! -path "*/node_modules/*" | while read -r tmpl; do
    out="${tmpl%.tmpl}"
    sed \
      -e "s|{{RCC_HOST}}|${RCC_HOST_PUBLIC}|g" \
      -e "s|{{GITHUB_USER}}|${GITHUB_USER_VAL}|g" \
      -e "s|{{AGENT_NAME}}|${AGENT_NAME}|g" \
      "$tmpl" > "$out"
    success "  rendered: $(basename "$out")"
  done
fi

# ═══════════════════════════════════════════════════════════════════════════
# CCC HOST: set up data dirs + optional service
# ═══════════════════════════════════════════════════════════════════════════
if $IS_RCC_HOST; then
  echo ""
  info "Setting up CCC host data directories..."
  DATA_DIR="$RCC_DIR/data"
  mkdir -p "$DATA_DIR/queue" "$DATA_DIR/agents" "$DATA_DIR/journal"
  success "Data dirs created: $DATA_DIR/{queue,agents,journal}"

  # Offer to install systemd service for the CCC API
  if [[ "$(uname)" == "Linux" ]] && command -v systemctl &>/dev/null; then
    echo ""
    ask "Install ccc-api.service (systemd) to auto-start the API? (y/n) [y]: "
    read -r INSTALL_SERVICE
    INSTALL_SERVICE="${INSTALL_SERVICE:-y}"
    if [ "$INSTALL_SERVICE" = "y" ]; then
      SERVICE_FILE="/etc/systemd/system/ccc-api.service"
      cat > /tmp/ccc-api.service << SVCEOF
[Unit]
Description=CCC API Server
After=network.target

[Service]
Type=simple
User=${USER}
WorkingDirectory=${WORKSPACE_DIR}
Environment="ENV_FILE=${ENV_FILE}"
ExecStart=/usr/bin/env bash -c 'set -a; source ${ENV_FILE}; set +a; exec node rcc/api/index.mjs'
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
SVCEOF
      if sudo cp /tmp/ccc-api.service "$SERVICE_FILE" && \
         sudo systemctl daemon-reload && \
         sudo systemctl enable rcc-api && \
         sudo systemctl start rcc-api; then
        success "ccc-api.service installed and started"
      else
        warn "Could not install systemd service — start manually: node rcc/api/index.mjs"
      fi
      rm -f /tmp/ccc-api.service
    fi
  elif [[ "$(uname)" == "Darwin" ]]; then
    echo ""
    ask "Install com.rcc.api LaunchAgent (auto-start on login)? (y/n) [y]: "
    read -r INSTALL_LAUNCH
    INSTALL_LAUNCH="${INSTALL_LAUNCH:-y}"
    if [ "$INSTALL_LAUNCH" = "y" ]; then
      PLIST="$HOME/Library/LaunchAgents/com.rcc.api.plist"
      NODE_BIN="$(command -v node)"
      cat > "$PLIST" << PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.rcc.api</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>-c</string>
    <string>set -a; source ${ENV_FILE}; set +a; exec ${NODE_BIN} ${WORKSPACE_DIR}/rcc/api/index.mjs</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>${RCC_DIR}/logs/rcc-api.log</string>
  <key>StandardErrorPath</key>
  <string>${RCC_DIR}/logs/rcc-api.log</string>
</dict>
</plist>
PLISTEOF
      mkdir -p "$RCC_DIR/logs"
      launchctl load "$PLIST" 2>/dev/null && success "com.rcc.api LaunchAgent installed" || warn "LaunchAgent install failed — load manually: launchctl load $PLIST"
    fi
  fi
fi

# ═══════════════════════════════════════════════════════════════════════════
# SUMMARY
# ═══════════════════════════════════════════════════════════════════════════
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e " ${GREEN}${BOLD}Done!${NC}  Agent: ${BOLD}${AGENT_NAME}${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "  Config:    $ENV_FILE"
echo "  Workspace: $WORKSPACE_DIR"
echo ""

if $IS_RCC_HOST; then
  echo "  This node is the CCC HOST."
  echo ""
  echo "  Next steps:"
  echo "  1. Start the API (if not auto-started):"
  echo "       node $WORKSPACE_DIR/rcc/api/index.mjs"
  echo ""
  echo "  2. Distribute your CCC_URL to client agents:"
  if [ -n "$RCC_HOST_PUBLIC" ]; then
    echo "       http://${RCC_HOST_PUBLIC}:${RCC_PORT}"
  else
    echo "       http://<this-host-ip>:${RCC_PORT}"
  fi
  echo ""
  echo "  3. Generate agent tokens and share with each client:"
  echo "       openssl rand -hex 32"
  echo ""
  echo "  4. Each client runs: bash deploy/rcc-init.sh"
else
  echo "  This node is a CLIENT pointing at: ${CCC_URL}"
  echo ""
  echo "  Next steps:"
  echo "  1. Verify connection:  curl ${CCC_URL}/health"
  echo "  2. Register this agent:"
  echo "       bash $WORKSPACE_DIR/deploy/register-agent.sh"
fi

echo ""
echo "  Run the pull agent manually to verify:"
echo "    bash $WORKSPACE_DIR/deploy/agent-pull.sh"
echo ""
echo "  Coding CLI turbocharger (if not already running):"
echo "    tmux new-session -d -s claude-main"
echo "    tmux send-keys -t claude-main 'claude --dangerously-skip-permissions' Enter"
echo ""
echo "  See deploy/README.md for full documentation."
echo ""
