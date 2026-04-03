#!/bin/bash
# CCC Hub — one-liner installer for Ubuntu/Debian VPS
# Usage: curl -fsSL https://raw.githubusercontent.com/jordanhubbard/CCC/main/rcc-hub/install-hub.sh | bash
#
# What this does:
#   1. Checks / installs Node.js 20+
#   2. Clones rcc-hub from GitHub (or updates if already present)
#   3. Installs npm dependencies
#   4. Runs .env setup wizard
#   5. Installs systemd service
#   6. Reminds you to open the firewall port

set -euo pipefail

REPO="https://github.com/jordanhubbard/CCC.git"
INSTALL_DIR="/opt/rcc-hub"
SERVICE_NAME="rcc-hub"
NODE_MIN_VERSION=18

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()    { echo -e "${GREEN}[rcc-hub]${NC} $*"; }
warn()    { echo -e "${YELLOW}[rcc-hub]${NC} $*"; }
error()   { echo -e "${RED}[rcc-hub]${NC} $*" >&2; exit 1; }

# ─── 0. Must run as root or with sudo ─────────────────────────────────────────
if [ "$EUID" -ne 0 ]; then
  warn "Not running as root — will use sudo for system steps."
  SUDO="sudo"
else
  SUDO=""
fi

# ─── 1. Node.js ───────────────────────────────────────────────────────────────
info "Checking Node.js..."
if command -v node &>/dev/null; then
  NODE_VERSION=$(node -e "process.exit(parseInt(process.version.slice(1)))" 2>/dev/null; echo $?)
  MAJOR=$(node -e "console.log(parseInt(process.version.slice(1)))")
  if [ "$MAJOR" -ge "$NODE_MIN_VERSION" ]; then
    info "Node.js $(node --version) already installed. ✓"
  else
    warn "Node.js $(node --version) is too old (need $NODE_MIN_VERSION+). Installing NodeSource repo..."
    curl -fsSL https://deb.nodesource.com/setup_20.x | $SUDO bash -
    $SUDO apt-get install -y nodejs
  fi
else
  info "Node.js not found. Installing via NodeSource..."
  if command -v apt-get &>/dev/null; then
    curl -fsSL https://deb.nodesource.com/setup_20.x | $SUDO bash -
    $SUDO apt-get install -y nodejs
  elif command -v yum &>/dev/null; then
    curl -fsSL https://rpm.nodesource.com/setup_20.x | $SUDO bash -
    $SUDO yum install -y nodejs
  else
    error "Cannot detect package manager. Install Node.js 20+ manually: https://nodejs.org"
  fi
fi

info "Node.js $(node --version), npm $(npm --version) ✓"

# ─── 2. Git ────────────────────────────────────────────────────────────────────
if ! command -v git &>/dev/null; then
  info "Installing git..."
  if command -v apt-get &>/dev/null; then
    $SUDO apt-get install -y git
  elif command -v yum &>/dev/null; then
    $SUDO yum install -y git
  fi
fi

# ─── 3. Clone or update ────────────────────────────────────────────────────────
if [ -d "$INSTALL_DIR/.git" ]; then
  info "Updating existing install at $INSTALL_DIR..."
  cd "$INSTALL_DIR"
  # Save .env if it exists
  [ -f .env ] && cp .env .env.bak && info "Backed up .env → .env.bak"
  git pull --ff-only origin main 2>/dev/null || git pull
elif [ -d "$INSTALL_DIR/src" ]; then
  info "Found existing rcc-hub at $INSTALL_DIR (not a git repo — skipping update)."
  cd "$INSTALL_DIR"
else
  info "Cloning rcc-hub to $INSTALL_DIR..."
  $SUDO mkdir -p "$INSTALL_DIR"
  # Make writable by current user
  CURRENT_USER=${SUDO_USER:-$(whoami)}
  $SUDO chown "$CURRENT_USER" "$INSTALL_DIR"
  git clone --depth=1 --filter=blob:none --sparse "$REPO" "$INSTALL_DIR"
  cd "$INSTALL_DIR"
  git sparse-checkout set rcc-hub
  # Move contents up if nested
  if [ -d "rcc-hub" ]; then
    cp -r rcc-hub/* . 2>/dev/null || true
    cp -r rcc-hub/.env* . 2>/dev/null || true
    rm -rf rcc-hub
  fi
fi

cd "$INSTALL_DIR"

# ─── 4. npm install ────────────────────────────────────────────────────────────
info "Installing npm dependencies..."
npm install --production --silent
info "Dependencies installed. ✓"

# ─── 5. .env setup ─────────────────────────────────────────────────────────────
if [ ! -f .env ]; then
  info "No .env found — running setup wizard..."
  echo ""
  node scripts/setup.mjs
else
  info ".env already exists — skipping wizard. Edit $INSTALL_DIR/.env to change settings."
fi

# ─── 6. Systemd service ────────────────────────────────────────────────────────
if command -v systemctl &>/dev/null; then
  info "Installing systemd service..."
  CURRENT_USER=${SUDO_USER:-$(whoami)}
  SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"

  $SUDO tee "$SERVICE_FILE" > /dev/null <<EOF
[Unit]
Description=CCC Hub — Claw Command Center
After=network.target
StartLimitIntervalSec=60
StartLimitBurst=5

[Service]
Type=simple
User=${CURRENT_USER}
WorkingDirectory=${INSTALL_DIR}
EnvironmentFile=${INSTALL_DIR}/.env
ExecStart=$(which node) src/api/index.mjs
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

  $SUDO systemctl daemon-reload
  $SUDO systemctl enable "$SERVICE_NAME"
  $SUDO systemctl restart "$SERVICE_NAME"

  sleep 2
  if systemctl is-active --quiet "$SERVICE_NAME"; then
    info "Service $SERVICE_NAME is running. ✓"
  else
    warn "Service may not have started. Check: journalctl -u $SERVICE_NAME -n 50"
  fi
else
  warn "systemd not found — skipping service install. Start manually with: cd $INSTALL_DIR && ./start.sh"
fi

# ─── 7. Firewall reminder ──────────────────────────────────────────────────────
RCC_PORT=$(grep '^RCC_PORT=' "$INSTALL_DIR/.env" 2>/dev/null | cut -d= -f2 || echo 8789)

echo ""
echo "════════════════════════════════════════════════════════════"
echo -e "${GREEN}  ✅  CCC Hub installed successfully!${NC}"
echo "════════════════════════════════════════════════════════════"
echo ""
echo "  Directory:  $INSTALL_DIR"
echo "  Service:    systemctl status $SERVICE_NAME"
echo "  Logs:       journalctl -u $SERVICE_NAME -f"
echo "  Config:     $INSTALL_DIR/.env"
echo ""
echo -e "${YELLOW}  ⚠️  FIREWALL: Make sure port $RCC_PORT is open:${NC}"
echo "    ufw allow $RCC_PORT/tcp"
echo "    # or for AWS/GCP/Azure: add inbound rule for port $RCC_PORT"
echo ""
echo "  Health check:"
echo "    curl http://localhost:$RCC_PORT/health"
echo ""
