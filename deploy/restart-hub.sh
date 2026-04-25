#!/usr/bin/env bash
# restart-hub.sh — rebuild acc-server on THIS node and restart it.
#
# Run this on the hub (not from a workstation). Requires sudo for the
# install to /usr/local/bin/acc-server.
set -euo pipefail

ACC_DIR="${ACC_DIR:-$HOME/.acc}"
LOG_DIR="${LOG_DIR:-$ACC_DIR/logs}"
WORKSPACE="${WORKSPACE:-$(git rev-parse --show-toplevel 2>/dev/null || true)}"

if [ -z "$WORKSPACE" ] || [ ! -f "${WORKSPACE}/Cargo.toml" ]; then
    echo "[restart-hub] ERROR: cannot locate workspace (run from inside the repo, or set WORKSPACE=)" >&2
    exit 1
fi

SERVER_BIN="${WORKSPACE}/target/release/acc-server"
SERVER_DEST="/usr/local/bin/acc-server"
mkdir -p "$LOG_DIR"

echo "[restart-hub] Building acc-server (release) from $WORKSPACE"
cargo build --release --manifest-path "${WORKSPACE}/Cargo.toml" -p acc-server

if [ ! -x "$SERVER_BIN" ]; then
    echo "[restart-hub] ERROR: build produced no binary at $SERVER_BIN" >&2
    exit 1
fi

echo "[restart-hub] Installing → $SERVER_DEST (sudo)"
sudo cp "$SERVER_BIN" "${SERVER_DEST}.new"
sudo mv "${SERVER_DEST}.new" "$SERVER_DEST"

echo "[restart-hub] Stopping any running acc-server"
sudo pkill -f "$SERVER_DEST" 2>/dev/null || true
sleep 1

echo "[restart-hub] Starting new acc-server (nohup, logs → ${LOG_DIR}/acc-server.log)"
# CCC-4cg: run from / so the daemon doesn't inherit the caller's cwd
# (e.g. ~/Src/ACC) — any relative-path writes would otherwise land in
# the user's git checkout instead of the hub's data dir.
( cd / && nohup "$SERVER_DEST" >> "${LOG_DIR}/acc-server.log" 2>&1 & disown || true )
sleep 2

if pgrep -f "$SERVER_DEST" >/dev/null; then
    PID=$(pgrep -f "$SERVER_DEST" | head -1)
    echo "[restart-hub] ✓ acc-server running (pid=$PID)"
else
    echo "[restart-hub] ✗ acc-server not running after restart — see ${LOG_DIR}/acc-server.log" >&2
    tail -20 "${LOG_DIR}/acc-server.log" >&2 || true
    exit 1
fi
