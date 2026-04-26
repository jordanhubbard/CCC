#!/usr/bin/env bash
# restart-agent.sh — rebuild acc-agent on THIS node and restart it.
#
# Verifies daemon manager (launchd/systemd) is configured and up-to-date
# BEFORE killing acc-agent, so the agent is never left dead due to a missing
# or stale daemon config.
set -euo pipefail

ACC_DIR="${ACC_DIR:-$HOME/.acc}"
LOG_DIR="${LOG_DIR:-$ACC_DIR/logs}"
WORKSPACE="${WORKSPACE:-$(git rev-parse --show-toplevel 2>/dev/null || true)}"

# Non-interactive SSH sessions don't source ~/.bashrc, so rustup-installed
# cargo isn't on PATH. Pull it in explicitly if it's there.
if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

if [ -z "$WORKSPACE" ] || [ ! -f "${WORKSPACE}/Cargo.toml" ]; then
    echo "[restart-agent] ERROR: cannot locate workspace (run from inside the repo, or set WORKSPACE=)" >&2
    exit 1
fi

AGENT_DEST="${ACC_DIR}/bin/acc-agent"
mkdir -p "$LOG_DIR" "${ACC_DIR}/bin"

# ── Preflight: ensure daemon manager is configured and current ────────────────
#
# We check and update the daemon config BEFORE killing acc-agent. That way
# if the config is missing or stale, we fix it first — so the kill is never
# a leap of faith.
ensure_daemon_configured() {
    if [[ "$(uname)" == "Darwin" ]]; then
        local PLIST_SRC="${WORKSPACE}/deploy/launchd/com.acc.agent.plist"
        local PLIST_DST="${HOME}/Library/LaunchAgents/com.acc.agent.plist"
        local rendered
        rendered=$(sed "s|AGENT_HOME|${HOME}|g" "${PLIST_SRC}")

        if [ ! -f "${PLIST_DST}" ]; then
            echo "[restart-agent] LaunchAgent not installed — installing now"
            echo "${rendered}" > "${PLIST_DST}"
            launchctl load "${PLIST_DST}" 2>/dev/null || true
            echo "[restart-agent] ✓ LaunchAgent installed"
        else
            local installed
            installed=$(cat "${PLIST_DST}")
            if [ "${rendered}" != "${installed}" ]; then
                echo "[restart-agent] LaunchAgent is stale — updating"
                launchctl unload "${PLIST_DST}" 2>/dev/null || true
                echo "${rendered}" > "${PLIST_DST}"
                launchctl load "${PLIST_DST}" 2>/dev/null || true
                echo "[restart-agent] ✓ LaunchAgent updated"
            else
                echo "[restart-agent] LaunchAgent is current"
            fi
        fi

        # Verify launchd will keep it alive
        if ! launchctl list 2>/dev/null | grep -q "com.acc.agent"; then
            echo "[restart-agent] WARNING: com.acc.agent not visible in launchctl list — may not auto-restart" >&2
        fi

    elif command -v systemctl &>/dev/null; then
        local SERVICE_SRC="${WORKSPACE}/deploy/systemd/acc-agent.service"
        local SERVICE_DST="/etc/systemd/system/acc-agent.service"
        local rendered
        rendered=$(sed "s|AGENT_USER|$(whoami)|g; s|AGENT_HOME|${HOME}|g" "${SERVICE_SRC}")

        if [ ! -f "${SERVICE_DST}" ]; then
            echo "[restart-agent] systemd unit not installed — installing now"
            if command -v sudo &>/dev/null; then
                echo "${rendered}" | sudo tee "${SERVICE_DST}" > /dev/null
                sudo systemctl daemon-reload
                sudo systemctl enable acc-agent
                echo "[restart-agent] ✓ systemd unit installed and enabled"
            else
                echo "[restart-agent] ERROR: no sudo access — cannot install systemd unit" >&2
                echo "    Run: sudo systemctl enable --now acc-agent" >&2
                exit 1
            fi
        else
            local installed
            installed=$(cat "${SERVICE_DST}")
            if [ "${rendered}" != "${installed}" ]; then
                echo "[restart-agent] systemd unit is stale — updating"
                if command -v sudo &>/dev/null; then
                    echo "${rendered}" | sudo tee "${SERVICE_DST}" > /dev/null
                    sudo systemctl daemon-reload
                    echo "[restart-agent] ✓ systemd unit updated"
                else
                    echo "[restart-agent] WARNING: no sudo access — cannot update systemd unit (continuing anyway)" >&2
                fi
            else
                echo "[restart-agent] systemd unit is current"
            fi
        fi

        # Verify the unit is enabled (will restart on exit)
        if ! systemctl is-enabled acc-agent &>/dev/null; then
            echo "[restart-agent] WARNING: acc-agent.service is not enabled — enabling now"
            sudo systemctl enable acc-agent 2>/dev/null || \
                echo "[restart-agent] WARNING: could not enable — agent may not restart after reboot" >&2
        fi
    else
        echo "[restart-agent] WARNING: no daemon manager detected (not macOS launchd, not systemd)" >&2
        echo "    The agent will NOT automatically restart after this script kills it." >&2
        echo "    Start manually after: nohup ${AGENT_DEST} supervise &" >&2
    fi
}

echo "[restart-agent] Checking daemon manager configuration..."
ensure_daemon_configured

# ── Build new acc-agent binary ─────────────────────────────────────────────
AGENT_BIN="${WORKSPACE}/target/release/acc-agent"

echo "[restart-agent] Building acc-agent (release) from $WORKSPACE"
cargo build --release --manifest-path "${WORKSPACE}/Cargo.toml" -p acc-agent

if [ ! -x "$AGENT_BIN" ]; then
    echo "[restart-agent] ERROR: build produced no binary at $AGENT_BIN" >&2
    exit 1
fi

echo "[restart-agent] Installing → $AGENT_DEST"
tmp="${AGENT_DEST}.new.$$"
cp "$AGENT_BIN" "$tmp"
chmod +x "$tmp"
mv "$tmp" "$AGENT_DEST"

# ── Kill and wait for respawn ──────────────────────────────────────────────
echo "[restart-agent] Stopping running acc-agent so the daemon manager respawns with the new binary"
# Capture old supervise PID before kill so we can confirm a NEW one comes up
old_sup=$(pgrep -f "acc-agent supervise" | head -1 || true)
pkill -x acc-agent 2>/dev/null || true

# launchd (macOS, KeepAlive=true) and systemd (Restart=on-failure) typically
# relaunch within 2-8 seconds. Poll up to ~30s for a NEW supervise process
# whose PID differs from the one we killed.
new_sup=""
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    sleep 2
    candidate=$(pgrep -f "acc-agent supervise" | head -1 || true)
    if [ -n "$candidate" ] && [ "$candidate" != "$old_sup" ]; then
        new_sup="$candidate"
        break
    fi
done

if [ -n "$new_sup" ]; then
    echo "[restart-agent] ✓ acc-agent supervise relaunched (pid=$new_sup, was pid=${old_sup:-none})"
else
    echo "[restart-agent] ✗ acc-agent supervise did NOT relaunch within 30s" >&2
    echo "    Daemon config was verified above — check daemon manager logs:" >&2
    if [[ "$(uname)" == "Darwin" ]]; then
        echo "    log show --predicate 'subsystem == \"com.apple.launchd\"' --last 2m" >&2
    else
        echo "    journalctl -u acc-agent -n 50" >&2
    fi
    echo "    Manual recovery: nohup ${AGENT_DEST} supervise &" >&2
    exit 1
fi
