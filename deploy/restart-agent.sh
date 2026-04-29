#!/usr/bin/env bash
# restart-agent.sh — rebuild acc-agent on THIS node and restart it.
#
# Verifies daemon manager (launchd/systemd) is configured and up-to-date
# BEFORE killing acc-agent, so the agent is never left dead due to a missing
# or stale daemon config.
#
# DNS-aware build strategy (bullwinkle / home-network nodes):
#   If external DNS is broken (can't resolve github.com/crates.io), cargo
#   fetch will time out and the build will fail.  We detect this early and
#   apply one of three mitigations in order:
#
#   1. Fix DNS via networksetup (macOS) — sets 1.1.1.1 + 8.8.8.8 and retries
#   2. Use a pre-built binary shipped in the repo at deploy/bin/acc-agent-<arch>
#   3. Pull source from rocky over Tailscale (jkh@100.89.199.14:Src/ACC) and
#      build --offline (all crates already fetched on rocky)
#
#   The rocky-remote path is documented in ~/.acc/.env as ROCKY_TAILSCALE_IP
#   and ROCKY_TAILSCALE_USER (defaults: 100.89.199.14 / jkh).
#
# macOS launchd notes
# -------------------
# `launchctl load/unload` before the binary swap can leave launchd racing on
# stale binaries. On Darwin, this script writes the LaunchAgent plist during
# preflight, but defers actual launchd registration/reload until after the new
# binary is atomically installed.
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

# Load .env for ROCKY_TAILSCALE_IP / ROCKY_TAILSCALE_USER overrides
# shellcheck disable=SC1090
source "${ACC_DIR}/.env" 2>/dev/null || true

# Rocky Tailscale coordinates (override in ~/.acc/.env if needed)
ROCKY_IP="${ROCKY_TAILSCALE_IP:-100.89.199.14}"
ROCKY_USER="${ROCKY_TAILSCALE_USER:-jkh}"
ROCKY_REPO="${ROCKY_TAILSCALE_REPO:-Src/ACC}"

# Darwin launchd action to perform after the binary swap:
# "none" | "register" | "reload"
_LAUNCHD_ACTION="none"

# ── DNS preflight ─────────────────────────────────────────────────────────────
#
# Probe external DNS with a 4-second timeout.  Returns 0 if reachable, 1 if not.
dns_ok() {
    # Try resolving github.com via the system resolver.
    # `getent hosts` works on Linux; `dscacheutil` on macOS.
    # Fall back to a curl head request with a tight timeout.
    if command -v getent >/dev/null 2>&1; then
        getent hosts github.com >/dev/null 2>&1 && return 0
    fi
    if command -v dscacheutil >/dev/null 2>&1; then
        dscacheutil -q host -a name github.com 2>/dev/null | grep -q "ip_address" && return 0
    fi
    # Last resort: curl with tight timeout
    curl -sf --max-time 4 --connect-timeout 4 \
        -o /dev/null "https://github.com" 2>/dev/null && return 0
    return 1
}

# Attempt to repair DNS on macOS by pointing all interfaces at 1.1.1.1 + 8.8.8.8.
fix_dns_macos() {
    echo "[restart-agent] Attempting DNS repair via networksetup..."
    local services
    services=$(networksetup -listallnetworkservices 2>/dev/null | tail -n +2 | grep -v '^\*') || true
    while IFS= read -r svc; do
        [ -z "$svc" ] && continue
        networksetup -setdnsservers "$svc" 1.1.1.1 8.8.8.8 2>/dev/null && \
            echo "[restart-agent]   ✓ DNS set on: $svc" || true
    done <<< "$services"
    # Flush DNS cache
    if command -v dscacheutil >/dev/null 2>&1; then
        dscacheutil -flushcache 2>/dev/null || true
    fi
    if command -v killall >/dev/null 2>&1; then
        killall -HUP mDNSResponder 2>/dev/null || true
    fi
    sleep 2
}

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
            echo "[restart-agent] LaunchAgent not installed — writing plist (will register after binary swap)"
            echo "${rendered}" > "${PLIST_DST}"
            _LAUNCHD_ACTION="register"
        else
            local installed
            installed=$(cat "${PLIST_DST}")
            if [ "${rendered}" != "${installed}" ]; then
                echo "[restart-agent] LaunchAgent is stale — updating plist (will reload after binary swap)"
                echo "${rendered}" > "${PLIST_DST}"
                _LAUNCHD_ACTION="reload"
            else
                echo "[restart-agent] LaunchAgent is current"
                _LAUNCHD_ACTION="none"
            fi
        fi

    elif command -v systemctl &>/dev/null; then
        # Hub nodes (Rocky) use acc-server.service which supervises acc-agent workers
        # directly. Agent nodes use acc-agent.service (supervise mode).
        # Detect which model is in use by checking acc-server.service first.
        if systemctl is-active acc-server.service &>/dev/null 2>&1; then
            echo "[restart-agent] acc-server.service is active — hub mode (acc-server manages workers)"
            return 0  # acc-server handles worker supervision; nothing to configure here
        fi

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

reload_launchd_after_binary_swap() {
    [[ "$(uname)" != "Darwin" ]] && return 0
    [[ "${_LAUNCHD_ACTION}" == "none" ]] && return 0

    local PLIST_DST="${HOME}/Library/LaunchAgents/com.acc.agent.plist"
    local GUI_TARGET="gui/$(id -u)"

    if launchctl bootout "${GUI_TARGET}" "${PLIST_DST}" 2>/dev/null; then
        echo "[restart-agent] launchctl bootout succeeded"
        sleep 1
        if launchctl bootstrap "${GUI_TARGET}" "${PLIST_DST}" 2>/dev/null; then
            echo "[restart-agent] ✓ LaunchAgent bootstrapped (new plist + new binary)"
        else
            echo "[restart-agent] WARNING: launchctl bootstrap failed; attempting legacy load" >&2
            launchctl load "${PLIST_DST}" 2>/dev/null || true
        fi
    else
        echo "[restart-agent] launchctl bootout unavailable or service not loaded — using legacy load"
        launchctl unload "${PLIST_DST}" 2>/dev/null || true
        launchctl load "${PLIST_DST}" 2>/dev/null || true
    fi

    if ! launchctl list 2>/dev/null | grep -q "com.acc.agent"; then
        echo "[restart-agent] WARNING: com.acc.agent not visible in launchctl list — may not auto-restart" >&2
    else
        echo "[restart-agent] ✓ com.acc.agent confirmed in launchctl list"
    fi
}

echo "[restart-agent] Checking daemon manager configuration..."
ensure_daemon_configured

# ── Build new acc-agent binary ─────────────────────────────────────────────
#
# Strategy (in order):
#   A. DNS is fine  → normal `cargo build --release`
#   B. DNS broken, macOS → fix via networksetup, retry once
#   C. Pre-built binary in repo  → copy it directly (no compile needed)
#   D. Rocky Tailscale reachable → rsync source + cargo build --offline
#   E. Nothing works             → abort with clear instructions

AGENT_BIN="${WORKSPACE}/target/release/acc-agent"
BUILD_DONE=false

# ── Detect host arch for pre-built binary lookup ──────────────────────────
HOST_ARCH="$(uname -m)"   # arm64 or x86_64
HOST_OS="$(uname -s | tr '[:upper:]' '[:lower:]')"  # darwin or linux
PREBUILT_BIN="${WORKSPACE}/deploy/bin/acc-agent-${HOST_OS}-${HOST_ARCH}"

do_cargo_build() {
    echo "[restart-agent] Building acc-agent (release) from $WORKSPACE"
    cargo build --release --manifest-path "${WORKSPACE}/Cargo.toml" -p acc-agent
    BUILD_DONE=true
}

# ── Path A: normal build if DNS is healthy ────────────────────────────────
echo "[restart-agent] Checking external DNS (github.com / crates.io)..."
if dns_ok; then
    echo "[restart-agent] DNS OK — proceeding with normal cargo build"
    do_cargo_build
fi

# ── Path B: macOS DNS repair + retry ─────────────────────────────────────
if ! $BUILD_DONE && [[ "$(uname)" == "Darwin" ]]; then
    echo "[restart-agent] DNS unreachable — attempting macOS DNS repair"
    fix_dns_macos
    if dns_ok; then
        echo "[restart-agent] DNS repaired — retrying cargo build"
        do_cargo_build
    else
        echo "[restart-agent] DNS still unreachable after repair attempt"
    fi
fi

# ── Path C: pre-built binary shipped in the repo ─────────────────────────
if ! $BUILD_DONE && [ -x "${PREBUILT_BIN}" ]; then
    echo "[restart-agent] Using pre-built binary: ${PREBUILT_BIN}"
    tmp="${AGENT_DEST}.new.$$"
    cp "${PREBUILT_BIN}" "$tmp"
    chmod +x "$tmp"
    mv "$tmp" "${AGENT_DEST}"
    echo "[restart-agent] ✓ Pre-built binary installed → ${AGENT_DEST}"
    # Skip the normal install step below — binary is already at AGENT_DEST
    BUILD_DONE=true
    SKIP_INSTALL=true
fi

# ── Path D: rocky Tailscale rsync + offline build ────────────────────────
if ! $BUILD_DONE; then
    echo "[restart-agent] Trying rocky Tailscale source sync (${ROCKY_USER}@${ROCKY_IP}:${ROCKY_REPO})"
    ROCKY_REACHABLE=false
    if ssh -o ConnectTimeout=8 -o BatchMode=yes \
           "${ROCKY_USER}@${ROCKY_IP}" true 2>/dev/null; then
        ROCKY_REACHABLE=true
    fi

    if $ROCKY_REACHABLE; then
        echo "[restart-agent] rocky reachable — rsyncing source tree..."
        # Sync source (excluding target/ to avoid huge binary blobs)
        rsync -az --delete \
            --exclude='target/' \
            --exclude='.git/' \
            "${ROCKY_USER}@${ROCKY_IP}:${ROCKY_REPO}/" \
            "${WORKSPACE}/"
        echo "[restart-agent] Source synced from rocky"

        # Also sync rocky's pre-fetched cargo registry so --offline works
        ROCKY_CARGO_REGISTRY="${ROCKY_USER}@${ROCKY_IP}:.cargo/registry"
        LOCAL_CARGO_REGISTRY="${HOME}/.cargo/registry"
        mkdir -p "${LOCAL_CARGO_REGISTRY}"
        echo "[restart-agent] Syncing cargo registry from rocky..."
        rsync -az \
            "${ROCKY_CARGO_REGISTRY}/" \
            "${LOCAL_CARGO_REGISTRY}/" 2>/dev/null || \
            echo "[restart-agent] WARNING: cargo registry sync failed — build may fail if crates missing"

        echo "[restart-agent] Building acc-agent --offline from synced source"
        cargo build --release --offline \
            --manifest-path "${WORKSPACE}/Cargo.toml" -p acc-agent
        BUILD_DONE=true
    else
        echo "[restart-agent] WARNING: rocky not reachable at ${ROCKY_IP}" >&2
    fi
fi

# ── Path E: all paths exhausted ──────────────────────────────────────────
if ! $BUILD_DONE; then
    cat >&2 <<EOF
[restart-agent] ERROR: cannot build acc-agent — all paths exhausted.

  DNS is broken and no fallback succeeded. Manual options:

  1. Fix DNS permanently (macOS):
       bash ${WORKSPACE}/deploy/fix-dns-bullwinkle.sh
     Then re-run: bash ${WORKSPACE}/deploy/restart-agent.sh

  2. Copy a pre-built binary from jordan's workstation:
       scp <jordan-mac>:~/Src/ACC/target/release/acc-agent \\
           ${AGENT_DEST}
     Then re-run: bash ${WORKSPACE}/deploy/restart-agent.sh

  3. SCP a pre-built binary for this platform (${HOST_OS}-${HOST_ARCH}):
       scp <source>:<path>/acc-agent ${AGENT_DEST}
     Then restart manually: pkill -x acc-agent || true

  4. Ensure rocky is reachable on Tailscale:
       ping ${ROCKY_IP}
     Then re-run this script (it will rsync source + registry from rocky).

  Rocky git remote: jkh@${ROCKY_IP}:${ROCKY_REPO}
EOF
    exit 1
fi

# ── Install binary (skip if Path C already wrote to AGENT_DEST) ──────────
SKIP_INSTALL="${SKIP_INSTALL:-false}"
if ! $SKIP_INSTALL; then
    if [ ! -x "$AGENT_BIN" ]; then
        echo "[restart-agent] ERROR: build produced no binary at $AGENT_BIN" >&2
        exit 1
    fi

    echo "[restart-agent] Installing → $AGENT_DEST"
    tmp="${AGENT_DEST}.new.$$"
    cp "$AGENT_BIN" "$tmp"
    chmod +x "$tmp"
    mv "$tmp" "$AGENT_DEST"
fi

echo "[restart-agent] Reloading daemon manager with new binary in place..."
reload_launchd_after_binary_swap

cleanup_gateway_processes() {
    # Remove native gateway children and legacy Python `hermes gateway`
    # processes before daemon-manager restart. Older migrations left both
    # orphaned on some hosts, causing duplicate Slack Socket Mode consumers.
    declare -a gateway_pids=()
    while IFS= read -r p; do
        [ -n "$p" ] && gateway_pids+=("$p")
    done < <(pgrep -f 'acc-agent +hermes +--gateway|hermes +gateway' 2>/dev/null || true)

    [ "${#gateway_pids[@]}" -gt 0 ] || return 0

    echo "[restart-agent] Stopping stale gateway process(es): ${gateway_pids[*]}"
    kill -TERM "${gateway_pids[@]}" 2>/dev/null || true
    sleep 2
    for p in "${gateway_pids[@]}"; do
        if kill -0 "$p" 2>/dev/null; then
            echo "[restart-agent] gateway pid=$p survived SIGTERM — sending SIGKILL"
            kill -KILL "$p" 2>/dev/null || true
        fi
    done
}

# ── Kill and wait for respawn ──────────────────────────────────────────────
echo "[restart-agent] Stopping running acc-agent so the daemon manager respawns with the new binary"
cleanup_gateway_processes

# Hub nodes (Rocky): acc-server.service owns the acc-agent workers.
# Kill the workers and the Rust supervisor in acc-server will respawn them
# with the new binary. No separate supervise process to look for.
if [[ "$(uname)" != "Darwin" ]] && command -v systemctl &>/dev/null \
    && systemctl is-active acc-server.service &>/dev/null 2>&1; then
    # Resolve PIDs upfront then signal by PID. The earlier
    # `pkill -f "acc-agent (bus|queue|tasks|hermes|proxy)"` form silently
    # left `hermes --gateway` and `slack-ingest` running on the previous
    # binary in production (they kept their old PIDs across restart),
    # so the new binary's tools never reached the gateway. PID-based
    # kill + verify + SIGKILL escalation is the robust path.
    declare -a child_pids=()
    while IFS= read -r p; do
        [ -n "$p" ] && child_pids+=("$p")
    done < <(pgrep -f 'acc-agent +(bus|queue|tasks|proxy|hermes|slack-ingest)([[:space:]]|$)' 2>/dev/null || true)

    if [ "${#child_pids[@]}" -gt 0 ]; then
        kill -TERM "${child_pids[@]}" 2>/dev/null || true
        # Up to ~10s for graceful exit before escalating.
        for _ in 1 2 3 4 5; do
            sleep 2
            still_alive=false
            for p in "${child_pids[@]}"; do
                if kill -0 "$p" 2>/dev/null; then still_alive=true; break; fi
            done
            $still_alive || break
        done
        for p in "${child_pids[@]}"; do
            if kill -0 "$p" 2>/dev/null; then
                echo "[restart-agent] pid=$p survived SIGTERM — sending SIGKILL"
                kill -KILL "$p" 2>/dev/null || true
            fi
        done
    fi

    # Wait for acc-server's Rust supervisor to respawn at least one worker.
    new_worker=""
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        sleep 2
        new_worker=$(pgrep -f "acc-agent (bus|queue|tasks)" | head -1 || true)
        [ -n "$new_worker" ] && break
    done
    if [ -n "$new_worker" ]; then
        echo "[restart-agent] ✓ acc-agent workers respawned by acc-server.service supervisor"
    else
        echo "[restart-agent] ✗ workers did not respawn within 20s — check: journalctl -u acc-server.service" >&2
        exit 1
    fi
    exit 0
fi

# Agent nodes: supervise mode (launchd / acc-agent.service / plain).
old_sup=$(pgrep -f "acc-agent supervise" | head -1 || true)

# Use `systemctl restart` on Linux: pkill sends SIGTERM which causes a clean
# exit (code 0) that Restart=on-failure won't recover from. systemctl restart
# uses SIGTERM too but marks the unit for immediate restart regardless of exit
# code. Fall back to pkill on macOS (launchd handles KeepAlive).
if [[ "$(uname)" != "Darwin" ]] && command -v systemctl &>/dev/null; then
    if systemctl is-active acc-agent &>/dev/null; then
        sudo systemctl restart acc-agent 2>/dev/null || \
            { echo "[restart-agent] systemctl restart failed — falling back to pkill" >&2; pkill -x acc-agent 2>/dev/null || true; }
    else
        # Unit isn't active — start it fresh so systemd takes ownership
        pkill -x acc-agent 2>/dev/null || true
        sudo systemctl start acc-agent 2>/dev/null || true
    fi
else
    pkill -x acc-agent 2>/dev/null || true
fi

# Poll up to ~30s for a NEW supervise process whose PID differs from the one
# we killed. launchd and systemd typically relaunch within 2-8 seconds.
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
