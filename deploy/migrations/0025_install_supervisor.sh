#!/usr/bin/env bash
# Migration 0025 — replace per-service units with single acc-agent supervise
#
# Linux:  installs /etc/systemd/system/acc-agent.service, removes old per-unit
#         files at both system (/etc/systemd/system/) AND user scope
#         (~/.config/systemd/user/) so both can't run simultaneously.
# macOS:  installs ~/Library/LaunchAgents/com.acc.agent.plist, removes old plists
#         using bootout/bootstrap (10.15+) with unload/load fallback (older).
#
# Restarts: acc-bus-listener
set -euo pipefail

ACC_DIR="${HOME}/.acc"
[[ -d "${HOME}/.acc" ]] || ACC_DIR="${HOME}/.ccc"
AGENT_HOME="${HOME}"
WORKSPACE="$(cd "$(dirname "$0")/../.." && pwd)"

log() { echo "[0025] $*"; }

# ── Linux ─────────────────────────────────────────────────────────────────────
if [[ "$(uname)" == "Linux" ]]; then
    SYS_UNITS=(
        acc-bus-listener.service
        acc-hermes-worker.service
        acc-nvidia-proxy.service
        acc-queue-worker.service
        acc-task-worker.service
        acc-server.service
        acc-agent.timer
    )

    # Tear down old system-level units
    log "disabling old system units..."
    for unit in "${SYS_UNITS[@]}"; do
        if systemctl is-enabled --quiet "${unit}" 2>/dev/null; then
            sudo systemctl disable --now "${unit}" 2>/dev/null && log "  disabled ${unit}" || true
        fi
        sudo rm -f \
            "/etc/systemd/system/${unit}" \
            "/etc/systemd/system/multi-user.target.wants/${unit}" \
            "/etc/systemd/system/timers.target.wants/${unit}" 2>/dev/null || true
    done

    # FIX #12: Tear down old user-level units so both scopes can't run simultaneously.
    # Older deployments (natasha-style) had these in ~/.config/systemd/user/.
    USER_UNITS=(
        acc-bus-listener.service
        acc-hermes-worker.service
        acc-nvidia-proxy.service
        acc-queue-worker.service
        acc-task-worker.service
        acc-agent.service
        hermes-gateway.service
    )
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    log "disabling old user units..."
    for unit in "${USER_UNITS[@]}"; do
        systemctl --user disable --now "${unit}" 2>/dev/null && log "  disabled user/${unit}" || true
        rm -f \
            "${HOME}/.config/systemd/user/${unit}" \
            "${HOME}/.config/systemd/user/default.target.wants/${unit}" \
            "${HOME}/.config/systemd/user/multi-user.target.wants/${unit}" 2>/dev/null || true
    done
    systemctl --user daemon-reload 2>/dev/null || true

    # Install new single supervisor unit
    log "installing acc-agent.service..."
    UNIT_DST="/etc/systemd/system/acc-agent.service"
    AGENT_USER="$(id -un)"
    mkdir -p "${ACC_DIR}/logs"

    sed \
        -e "s|AGENT_HOME|${AGENT_HOME}|g" \
        -e "s|AGENT_USER|${AGENT_USER}|g" \
        "${WORKSPACE}/deploy/systemd/acc-agent.service" \
        | sudo tee "${UNIT_DST}" > /dev/null

    sudo systemctl daemon-reload
    sudo systemctl enable --now acc-agent.service
    log "acc-agent.service enabled and started"

# ── macOS ──────────────────────────────────────────────────────────────────────
elif [[ "$(uname)" == "Darwin" ]]; then
    LAUNCH_AGENTS="${HOME}/Library/LaunchAgents"
    UID_NUM="$(id -u)"

    OLD_PLISTS=(
        com.acc.bus-listener.plist
        com.acc.hermes-worker.plist
        com.acc.nvidia-proxy.plist
        com.acc.queue-worker.plist
        com.acc.task-worker.plist
        com.acc.exec-listen.plist
        ai.hermes.gateway.plist
    )

    # FIX #10: Use bootout/bootstrap (10.15+) with unload/load as fallback.
    log "unloading old plists..."
    for plist in "${OLD_PLISTS[@]}"; do
        full="${LAUNCH_AGENTS}/${plist}"
        label="${plist%.plist}"
        if [[ -f "${full}" ]]; then
            # Try modern bootout first, fall back to legacy unload
            launchctl bootout "gui/${UID_NUM}/${label}" 2>/dev/null \
                || launchctl unload -w "${full}" 2>/dev/null \
                || true
            rm -f "${full}"
            log "  removed ${plist}"
        fi
    done

    # Install new plist
    log "installing com.acc.agent.plist..."
    mkdir -p "${LAUNCH_AGENTS}"
    mkdir -p "${ACC_DIR}/logs"
    PLIST_DST="${LAUNCH_AGENTS}/com.acc.agent.plist"

    sed "s|AGENT_HOME|${AGENT_HOME}|g" \
        "${WORKSPACE}/deploy/launchd/com.acc.agent.plist" > "${PLIST_DST}"

    # Try modern bootstrap first, fall back to legacy load
    launchctl bootstrap "gui/${UID_NUM}" "${PLIST_DST}" 2>/dev/null \
        || launchctl load -w "${PLIST_DST}"
    log "com.acc.agent.plist loaded"

else
    log "WARNING: unsupported OS $(uname) — no units installed"
fi

log "done"
