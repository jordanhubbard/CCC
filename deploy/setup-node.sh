#!/bin/bash
# setup-node.sh — Bootstrap a new ACC agent node
# Run once on a new machine. Safe to re-run (idempotent).
#
# Usage:
#   REPO_URL=git@github.com:yourorg/your-ccc-repo.git bash deploy/setup-node.sh
#   OR (from inside an existing clone): bash deploy/setup-node.sh
#
# Tip: run deploy/acc-init.sh after this to configure your .env interactively.

set -e

ACC_OWNER="${ACC_OWNER:-jordanhubbard}"
ACC_REPO="${ACC_REPO:-ACC}"
REPO_URL="${REPO_URL:-git@github.com:${ACC_OWNER}/${ACC_REPO}.git}"
ACC_DIR="$HOME/.acc"
WORKSPACE="$ACC_DIR/workspace"
ENV_FILE="$ACC_DIR/.env"
LOG_DIR="$ACC_DIR/logs"
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
mkdir -p "$ACC_DIR" "$LOG_DIR"
success "Created $ACC_DIR"

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
  echo "  │  Required: AGENT_NAME, AGENT_HOST, ACC_URL"
  echo "  │            ACC_AGENT_TOKEN, NVIDIA_API_KEY"
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
  # On macOS, the "pull cron" is the acc-agent LaunchAgent itself (KeepAlive=true).
  # acc-agent supervise spawns hermes, the bus listener, and the queue worker as
  # children; the bus listener handles acc.update → agent-pull.sh on demand.
  # There is no separate periodic cron on macOS.
  PLIST_SRC="$WORKSPACE/deploy/launchd/com.acc.agent.plist"
  PLIST_DST="$HOME/Library/LaunchAgents/com.acc.agent.plist"
  if [ -f "$PLIST_DST" ]; then
    warn "LaunchAgent already installed at $PLIST_DST — reloading"
    launchctl unload "$PLIST_DST" 2>/dev/null || true
  fi
  # Substitute AGENT_HOME placeholder with the actual home directory.
  sed "s|AGENT_HOME|$HOME|g" "$PLIST_SRC" > "$PLIST_DST"
  launchctl load "$PLIST_DST"
  success "LaunchAgent installed and loaded (acc-agent supervise)"
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
  local CRON_FRAGMENT="$WORKSPACE/deploy/crontab-acc.txt"
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
  warn "Unknown platform — install ops crons manually from deploy/crontab-acc.txt"
fi

# ── Install node dependencies ─────────────────────────────────────────────
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

# python3-venv (required to create the hermes venv; Debian/Ubuntu splits this out)
if [[ "$PLATFORM" == "linux" ]] && ! python3 -m venv --help &>/dev/null 2>&1; then
  info "Installing python3-venv..."
  PY_VER=$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')
  sudo apt-get install -y "python${PY_VER}-venv" 2>/dev/null || \
    warn "Could not auto-install python${PY_VER}-venv. Run: sudo apt-get install -y python${PY_VER}-venv"
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
# DEPRECATED: Python hermes-agent has been replaced by native Rust acc-agent.
# The following install logic is kept for reference during transition only.
# acc-agent hermes --poll and acc-agent hermes --gateway replace 'hermes' entirely.
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

# Install acc-node skill into Hermes
CCC_SKILL_SRC="$WORKSPACE/skills/acc-node"
if [ -d "$CCC_SKILL_SRC" ] && [ "$HERMES_INSTALLED" = true ]; then
  SKILL_DEST="$HOME/.hermes/skills/acc-node"
  if [ ! -d "$SKILL_DEST" ]; then
    cp -r "$CCC_SKILL_SRC" "$SKILL_DEST"
    success "acc-node skill installed into Hermes"
  else
    success "acc-node skill already in Hermes"
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
- This agent is part of the CCC fleet managed by ~/Src/CCC
- Full workspace docs live in ~/Src/CCC/AGENTS.md — read on first boot
- Config: ~/.acc/.env — ACC_URL and ACC_AGENT_TOKEN are required before starting
- Workqueue: ~/Src/CCC/workqueue/queue.json — polled each heartbeat cycle
- Secrets are stored in TokenHub (~/Src/tokenhub); never hardcode credentials

### Companion Repos
Clone these on any new command node without waiting to be asked:
- ~/Src/CCC       — core fleet repo (already present)
- ~/Src/tokenhub  — required: LLM router and secret store

Minimum viable command node: CCC + tokenhub.

## Beliefs

### Fleet Operational Conventions
<!-- confidence: 0.8 | updated: SEED_DATE -->
- Register every secret in TokenHub BEFORE starting any service
<!-- confidence: 0.8 | updated: SEED_DATE -->
- Session startup order: SOUL.md → USER.md → memory/YYYY-MM-DD.md → MEMORY.md (main session only)
<!-- confidence: 0.7 | updated: SEED_DATE -->
- New command nodes need CCC + tokenhub as a minimum before any other work

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

# ── Rust toolchain ───────────────────────────────────────────────────────
# Rust is required to build acc-server and other fleet binaries from source.
info "Checking Rust toolchain..."
export PATH="$HOME/.cargo/bin:$PATH"
if command -v rustc &>/dev/null; then
  success "Rust already installed: $(rustc --version)"
else
  info "Installing Rust via rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  export PATH="$HOME/.cargo/bin:$PATH"
  if command -v rustc &>/dev/null; then
    success "Rust installed: $(rustc --version)"
  else
    warn "Rust install may have failed — check ~/.cargo/bin manually"
  fi
fi

# ── Beads (bd) — required repo dependency ────────────────────────────────
# Beads is needed on every node so the hub can read project .beads databases
# and turn issues into queue tasks. Source: https://github.com/gastownhall/beads
BEADS_SRC="${BEADS_SRC:-$HOME/Src/beads}"
BEADS_REPO="https://github.com/gastownhall/beads.git"

info "Checking beads (bd) issue tracker..."

if command -v bd &>/dev/null 2>&1 || [ -x "$HOME/.local/bin/bd" ]; then
  BD_VER=$(bd --version 2>/dev/null | head -1 || "$HOME/.local/bin/bd" --version 2>/dev/null | head -1 || echo "unknown")
  success "beads already installed: $BD_VER"
else
  info "beads not found — installing from $BEADS_REPO"

  # Ensure Go is available (beads is a Go project)
  if ! command -v go &>/dev/null; then
    warn "Go not found — cannot build beads from source."
    echo ""
    echo "  ┌─────────────────────────────────────────────────────────────┐"
    echo "  │  Install Go, then re-run setup-node.sh to install beads.    │"
    echo "  │    Linux:  https://go.dev/dl/ or: snap install go --classic │"
    echo "  │    macOS:  brew install go                                   │"
    echo "  │  Then: cd ~/Src/beads && make install-force                  │"
    echo "  └─────────────────────────────────────────────────────────────┘"
    echo ""
  else
    # Clone if not present
    if [ ! -d "$BEADS_SRC/.git" ]; then
      mkdir -p "$(dirname "$BEADS_SRC")"
      git clone --quiet "$BEADS_REPO" "$BEADS_SRC" 2>/dev/null \
        && info "beads cloned to $BEADS_SRC" \
        || { warn "Failed to clone beads from $BEADS_REPO"; }
    else
      info "beads source already at $BEADS_SRC — pulling latest"
      git -C "$BEADS_SRC" pull --quiet --ff-only 2>/dev/null || true
    fi

    # Build and install
    if [ -d "$BEADS_SRC" ] && [ -f "$BEADS_SRC/Makefile" ]; then
      mkdir -p "$HOME/.local/bin"
      if (cd "$BEADS_SRC" && make install-force) 2>/dev/null; then
        BD_VER=$(bd --version 2>/dev/null | head -1 || "$HOME/.local/bin/bd" --version 2>/dev/null | head -1 || echo "unknown")
        success "beads installed: $BD_VER"
      else
        warn "beads build failed — check $BEADS_SRC for errors"
      fi
    fi
  fi
fi

# ── AgentFS mount ────────────────────────────────────────────────────────
# AgentFS is served from Rocky (100.89.199.14) as a Samba share named 'accfs'.
# The share exports /srv/accfs/shared. Agents mount it at ~/.acc/shared.
#
# Credentials are stored in /etc/samba/smbcredentials (root-owned, 600).
# If ACC_SMB_PASSWORD is set in the environment, it will be used; otherwise
# the setup prints instructions for manual credential configuration.
AGENTFS_HOST="${AGENTFS_HOST:-100.89.199.14}"
AGENTFS_SHARE="accfs"
AGENTFS_USER="${AGENTFS_USER:-jkh}"
AGENTFS_MOUNT="${AGENTFS_MOUNT:-$HOME/.acc/shared}"
AGENTFS_CREDS="/etc/samba/smbcredentials"

info "Checking AgentFS mount..."

_agentfs_mounted() {
  mount | grep -q "${AGENTFS_HOST}/${AGENTFS_SHARE}" 2>/dev/null
}

_agentfs_healthy() {
  _agentfs_mounted && ls "$AGENTFS_MOUNT" >/dev/null 2>&1
}

_agentfs_is_hub_resident() {
  # The hub serves AgentFS from /srv/accfs/shared. On the hub itself,
  # we don't mount our own export — we symlink the shared dir into
  # ~/.acc/shared/<slug> so agents looking up workspaces find them.
  [ -d /srv/accfs/shared ] && ! mount | grep -q "on /srv/accfs" 2>/dev/null
}

if _agentfs_healthy; then
  success "AgentFS already mounted and healthy at $AGENTFS_MOUNT"
elif _agentfs_is_hub_resident; then
  # Hub-resident agent (e.g. rocky on do-host1): /srv/accfs/shared/ is a
  # real local directory, not a mount. Mounting our own SMB export back
  # at $HOME/.acc/shared causes a self-loop. Symlink each project's
  # shared dir directly so workspace lookups resolve to the real path.
  info "Hub-resident detected — wiring AgentFS via symlink farm (no SMB mount needed)"
  mkdir -p "$AGENTFS_MOUNT"
  _linked=0
  for _src in /srv/accfs/shared/*/; do
    _slug=$(basename "$_src")
    _dst="$AGENTFS_MOUNT/$_slug"
    if [ ! -e "$_dst" ]; then
      ln -s "$_src" "$_dst" && _linked=$((_linked+1))
    fi
  done
  success "AgentFS symlink farm: $_linked new link(s) added at $AGENTFS_MOUNT"
else
  mkdir -p "$AGENTFS_MOUNT"

  if [[ "$PLATFORM" == "linux" ]]; then
    # Write credentials file if password is known
    if [[ -n "${ACC_SMB_PASSWORD:-}" ]]; then
      if sudo -n true 2>/dev/null; then
        sudo bash -c "cat > $AGENTFS_CREDS << EOF
username=$AGENTFS_USER
password=$ACC_SMB_PASSWORD
EOF
chmod 600 $AGENTFS_CREDS"
      fi
    fi

    if [ ! -f "$AGENTFS_CREDS" ]; then
      warn "AgentFS credentials not found at $AGENTFS_CREDS"
      echo ""
      echo "  ┌──────────────────────────────────────────────────────────────────┐"
      echo "  │  Create /etc/samba/smbcredentials (root-owned, chmod 600):       │"
      echo "  │    username=jkh                                                   │"
      echo "  │    password=<ACC Samba password from Rocky>                       │"
      echo "  │  Then re-run setup-node.sh or install the mount unit manually.   │"
      echo "  └──────────────────────────────────────────────────────────────────┘"
      echo ""
    else
      # Install systemd mount unit
      if command -v systemctl &>/dev/null; then
        _uid=$(id -u)
        _gid=$(id -g)
        _unit_name=$(systemd-escape --path "$AGENTFS_MOUNT").mount
        _unit_file="/etc/systemd/system/${_unit_name}"

        if [ ! -f "$_unit_file" ] && sudo -n true 2>/dev/null; then
          sudo bash -c "cat > $_unit_file << EOF
[Unit]
Description=ACC shared filesystem (Rocky Samba/SMB)
After=network-online.target
Wants=network-online.target

[Mount]
What=//${AGENTFS_HOST}/${AGENTFS_SHARE}
Where=${AGENTFS_MOUNT}
Type=cifs
Options=credentials=${AGENTFS_CREDS},uid=${_uid},gid=${_gid},file_mode=0664,dir_mode=0775,_netdev,vers=3.0,nofail

[Install]
WantedBy=multi-user.target
EOF"
          sudo systemctl daemon-reload
          sudo systemctl enable --now "$_unit_name" && \
            success "AgentFS mount unit installed and started" || \
            warn "AgentFS mount unit install failed"
        elif [ -f "$_unit_file" ]; then
          # Unit exists; ensure it's mounted
          sudo -n systemctl start "$_unit_name" 2>/dev/null && \
            success "AgentFS mount started" || true
        fi
      fi

      # Verify
      if _agentfs_healthy; then
        success "AgentFS mounted at $AGENTFS_MOUNT: $(ls $AGENTFS_MOUNT | tr '\n' ' ')"
      else
        warn "AgentFS mount attempted but not healthy — check /etc/samba/smbcredentials and network"
      fi
    fi

  elif [[ "$PLATFORM" == "macos" ]]; then
    # macOS: mount_smbfs + LaunchAgent for persistence.
    # Without ACC_SMB_PASSWORD, mount_smbfs falls back to anonymous and
    # the server rejects with "Authentication error". Surface this
    # clearly instead of silently installing a plist that fails forever.
    if [[ -z "${ACC_SMB_PASSWORD:-}" ]]; then
      warn "macOS AgentFS mount needs ACC_SMB_PASSWORD."
      echo ""
      echo "  ┌──────────────────────────────────────────────────────────────────┐"
      echo "  │  macOS doesn't read /etc/samba/smbcredentials. To enable the    │"
      echo "  │  AgentFS mount on this Mac:                                      │"
      echo "  │                                                                  │"
      echo "  │    1. Get the Samba password from the hub (/etc/samba/...)      │"
      echo "  │    2. Add to ~/.acc/.env (or ~/.zshrc) before re-running setup: │"
      echo "  │         ACC_SMB_PASSWORD='<password>'                            │"
      echo "  │    3. Or store it in macOS Keychain and use a credential        │"
      echo "  │       reference in the LaunchAgent (more secure).                │"
      echo "  │                                                                  │"
      echo "  │  Without this, agent task workspaces will be empty stubs and    │"
      echo "  │  any task this agent claims will silently fail to do real work. │"
      echo "  └──────────────────────────────────────────────────────────────────┘"
      echo ""
    else
      _plist="$HOME/Library/LaunchAgents/com.acc.accfs-mount.plist"
      if [ ! -f "$_plist" ]; then
        _pw_fragment="${AGENTFS_USER}:${ACC_SMB_PASSWORD}@"
        cat > "$_plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>             <string>com.acc.accfs-mount</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>-c</string>
        <string>mkdir -p ${AGENTFS_MOUNT} &amp;&amp; /sbin/mount_smbfs //${_pw_fragment}${AGENTFS_HOST}/${AGENTFS_SHARE} ${AGENTFS_MOUNT} 2&gt;&gt; $HOME/.acc/logs/accfs-mount.log || true</string>
    </array>
    <key>RunAtLoad</key>   <true/>
    <key>KeepAlive</key>   <false/>
    <key>StandardOutPath</key> <string>$HOME/.acc/logs/accfs-mount.log</string>
    <key>StandardErrorPath</key> <string>$HOME/.acc/logs/accfs-mount.log</string>
</dict>
</plist>
EOF
        launchctl load "$_plist" 2>/dev/null
        sleep 2
        success "AgentFS LaunchAgent installed"
      else
        success "AgentFS LaunchAgent already present"
      fi
      # Verify (note: ls may fail from SSH due to macOS TCC; use mount + stat)
      if _agentfs_mounted; then
        success "AgentFS mounted at $AGENTFS_MOUNT (macOS TCC may block ls from SSH — normal)"
      else
        warn "AgentFS not mounted — check ~/Library/LaunchAgents/com.acc.accfs-mount.plist and logs"
      fi
    fi
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
  SERVICE_SRC="$WORKSPACE/deploy/systemd/acc-agent.service"
  SERVICE_DST="/etc/systemd/system/acc-agent.service"
  _rendered=$(sed "s|AGENT_USER|$(whoami)|g; s|AGENT_HOME|$HOME|g" "$SERVICE_SRC")
  if [ -f "$SERVICE_DST" ]; then
    _installed=$(cat "$SERVICE_DST")
    if [ "$_rendered" = "$_installed" ]; then
      success "acc-agent.service already up-to-date"
    else
      warn "acc-agent.service exists but is stale — updating"
      if [ -w "/etc/systemd/system" ] || command -v sudo &>/dev/null; then
        echo "$_rendered" | sudo tee "$SERVICE_DST" > /dev/null && \
        sudo systemctl daemon-reload && \
        sudo systemctl restart acc-agent && \
        success "acc-agent.service updated and restarted" || \
        warn "Could not update systemd service (needs sudo)"
      else
        warn "No sudo access — update systemd service manually"
      fi
    fi
  else
    if [ -w "/etc/systemd/system" ] || command -v sudo &>/dev/null; then
      echo "$_rendered" | sudo tee "$SERVICE_DST" > /dev/null && \
      sudo systemctl daemon-reload && \
      sudo systemctl enable acc-agent && \
      sudo systemctl start acc-agent && \
      success "acc-agent.service installed and started" || \
      warn "Could not install systemd service (needs sudo). Run manually: sudo systemctl enable --now acc-agent"
    else
      warn "No sudo access — install systemd service manually: sudo systemctl enable --now acc-agent"
    fi
  fi
fi

# ── Write onboarding signature ────────────────────────────────────────────
if [ ! -f "$ACC_DIR/agent.json" ]; then
  CCC_VERSION=$(cd "$WORKSPACE" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")
  _AGENT_BIN="${CCC_AGENT:-$ACC_DIR/bin/ccc-agent}"
  [ ! -x "$_AGENT_BIN" ] && _AGENT_BIN="$(command -v ccc-agent 2>/dev/null || echo "")"

  if [ -x "$_AGENT_BIN" ]; then
    "$_AGENT_BIN" agent init "$ACC_DIR/agent.json" \
      --name="${AGENT_NAME:-unknown}" \
      --host="$(hostname)" \
      --version="$CCC_VERSION" \
      --by="setup-node.sh" \
      && success "Onboarding signature written to $ACC_DIR/agent.json" \
      || warn "Failed to write agent.json (non-fatal)"
  elif command -v python3 >/dev/null 2>&1; then
    python3 - "$ACC_DIR/agent.json" "${AGENT_NAME:-unknown}" "$(hostname)" "$CCC_VERSION" << 'PYEOF'
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
    success "Onboarding signature written to $ACC_DIR/agent.json"
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
echo "  See README.md for details."
echo ""
