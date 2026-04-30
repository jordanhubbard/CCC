#!/usr/bin/env bash
# restart-hub.sh — rebuild acc-server + acc-agent on THIS hub node and restart
# via systemd (acc-server.service).
#
# Run this on the hub (not from a workstation). Requires sudo for installs
# to /usr/local/bin and systemctl commands.
#
# What this does:
#   1. Build release binaries for acc-server and acc-agent
#   2. Atomically install them to their canonical paths
#   3. Sync the acc-server.service unit file from the repo template
#   4. Stop acc-agent.service (acc-server will supervise the children directly)
#   5. Enable + restart acc-server.service — systemd becomes the sole supervisor
set -euo pipefail

ACC_DIR="${ACC_DIR:-$HOME/.acc}"
WORKSPACE="${WORKSPACE:-$(git rev-parse --show-toplevel 2>/dev/null || true)}"

# Source cargo env when invoked from a non-login shell (e.g. ssh + bash -c)
if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

if [ -z "$WORKSPACE" ] || [ ! -f "${WORKSPACE}/Cargo.toml" ]; then
    echo "[restart-hub] ERROR: cannot locate workspace (run from inside repo, or set WORKSPACE=)" >&2
    exit 1
fi

# Load and refresh secrets before the systemd restart so EnvironmentFile and
# ~/.acc/acc.json reflect all workspace gateways.
# shellcheck disable=SC1090
source "${ACC_DIR}/.env" 2>/dev/null || true

if [ -x "${WORKSPACE}/deploy/secrets-sync.sh" ]; then
    echo "[restart-hub] Syncing secrets into ${ACC_DIR}/.env"
    ACC_DIR="${ACC_DIR}" bash "${WORKSPACE}/deploy/secrets-sync.sh" --force || \
        echo "[restart-hub] WARNING: secrets sync failed — continuing with cached .env" >&2
    # shellcheck disable=SC1090
    source "${ACC_DIR}/.env" 2>/dev/null || true
fi

if [ -x "${WORKSPACE}/deploy/reconcile-hub-supervisor.sh" ]; then
    echo "[restart-hub] Reconciling hub supervisor process list"
    ACC_DIR="${ACC_DIR}" bash "${WORKSPACE}/deploy/reconcile-hub-supervisor.sh" || \
        echo "[restart-hub] WARNING: hub supervisor reconciliation failed" >&2
fi

SERVER_BIN="${WORKSPACE}/target/release/acc-server"
AGENT_BIN="${WORKSPACE}/target/release/acc-agent"
SERVER_DEST="/usr/local/bin/acc-server"
AGENT_DEST="${ACC_DIR}/bin/acc-agent"
UNIT_TEMPLATE="${WORKSPACE}/deploy/systemd/acc-server.service"
UNIT_DEST="/etc/systemd/system/acc-server.service"

mkdir -p "${ACC_DIR}/bin"

install_hermes_alias() {
    ln -sf acc-agent "${ACC_DIR}/bin/hermes"
    echo "[restart-hub] ✓ hermes compatibility command → ${ACC_DIR}/bin/hermes"
}

# ── 1. Build ─────────────────────────────────────────────────────────────────
echo "[restart-hub] Building acc-server + acc-agent (release) from $WORKSPACE"
cargo build --release --manifest-path "${WORKSPACE}/Cargo.toml" -p acc-server -p acc-agent

for bin in "$SERVER_BIN" "$AGENT_BIN"; do
    [ -x "$bin" ] || { echo "[restart-hub] ERROR: build produced no binary at $bin" >&2; exit 1; }
done

# ── 2. Install binaries atomically ───────────────────────────────────────────
echo "[restart-hub] Installing → $SERVER_DEST (sudo)"
sudo cp "$SERVER_BIN" "${SERVER_DEST}.new"
sudo mv "${SERVER_DEST}.new" "$SERVER_DEST"

echo "[restart-hub] Installing → $AGENT_DEST"
tmp="${AGENT_DEST}.new.$$"
cp "$AGENT_BIN" "$tmp"
chmod +x "$tmp"
mv "$tmp" "$AGENT_DEST"
install_hermes_alias

# ── 3. Sync systemd unit ─────────────────────────────────────────────────────
if [ -f "$UNIT_TEMPLATE" ]; then
    rendered=$(sed "s|AGENT_USER|$(whoami)|g; s|AGENT_HOME|${HOME}|g" "$UNIT_TEMPLATE")
    installed=$(cat "$UNIT_DEST" 2>/dev/null || true)
    if [ "$rendered" != "$installed" ]; then
        echo "[restart-hub] Updating $UNIT_DEST from template"
        echo "$rendered" | sudo tee "$UNIT_DEST" > /dev/null
        sudo systemctl daemon-reload
    else
        echo "[restart-hub] acc-server.service unit is current"
    fi
fi

# ── 4. Stop acc-agent.service (acc-server owns the children now) ──────────────
if systemctl is-active acc-agent.service &>/dev/null; then
    echo "[restart-hub] Stopping acc-agent.service (acc-server will supervise children)"
    sudo systemctl stop acc-agent.service
fi
sudo systemctl disable acc-agent.service 2>/dev/null || true

# ── 5. Enable + restart acc-server.service ────────────────────────────────────
sudo systemctl enable acc-server.service
echo "[restart-hub] Restarting acc-server.service"
sudo systemctl restart acc-server.service

# Wait for the service to settle
for i in 1 2 3 4 5 6; do
    sleep 2
    if systemctl is-active acc-server.service &>/dev/null; then
        PID=$(systemctl show -p MainPID --value acc-server.service 2>/dev/null || pgrep -f "$SERVER_DEST" | head -1)
        echo "[restart-hub] ✓ acc-server.service active (pid=$PID)"
        exit 0
    fi
done

echo "[restart-hub] ✗ acc-server.service failed to start" >&2
systemctl status acc-server.service --no-pager -n 30 >&2 || true
exit 1
