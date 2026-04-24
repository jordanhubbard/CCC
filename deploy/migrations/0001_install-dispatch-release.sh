#!/usr/bin/env bash
# Description: Install acc-agent binary with dispatch bus-wake (T4) and restart task/queue workers
# Restarts: acc-task-worker acc-queue-worker acc-bus-listener
#
# Context: Commits c76a406 and 0287777 add the auto-dispatch loop and agent-side
# bus subscriber. acc-agent now subscribes to /bus/stream SSE and wakes the task
# poll loop immediately on tasks:dispatch_nudge / tasks:dispatch_assigned instead
# of waiting up to 30s. The new binary must be installed before the service
# restarts declared above take effect.
# Condition: all agent nodes (linux + macos)

DEST_BIN="${ACC_DIR:-${HOME}/.acc}/bin/acc-agent"
WORKSPACE="${WORKSPACE:-${HOME}/.acc/workspace}"

# Pick the right binary from the workspace root
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)   SRC="${WORKSPACE}/acc-agent-linux-amd64" ;;
    Darwin-arm64)   SRC="${WORKSPACE}/acc-agent" ;;
    Darwin-x86_64)  SRC="${WORKSPACE}/acc-agent" ;;
    *)              SRC="${WORKSPACE}/acc-agent" ;;
esac

install_binary() {
    local src="$1"
    mkdir -p "$(dirname "${DEST_BIN}")"
    TMP="${DEST_BIN}.new.$$"
    cp "${src}" "${TMP}"
    chmod +x "${TMP}"
    mv "${TMP}" "${DEST_BIN}"
    m_success "Installed $(basename "${src}") → ${DEST_BIN}"
}

if [ "${DRY_RUN:-false}" = "true" ]; then
    m_info "[dry-run] would install acc-agent → ${DEST_BIN}"
elif [ -f "${SRC}" ] && file "${SRC}" | grep -q "$(uname -m)"; then
    # Pre-built binary matches this platform
    install_binary "${SRC}"
elif command -v cargo &>/dev/null && [ -d "${WORKSPACE}/agent" ]; then
    # Fall back: build from source (fleet nodes have Rust via setup-node.sh)
    m_info "No matching pre-built binary — building acc-agent from source (this takes ~2min)"
    export PATH="${HOME}/.cargo/bin:${PATH}"
    if cargo build --release --manifest-path "${WORKSPACE}/agent/Cargo.toml" \
            --quiet 2>>"${LOG_DIR:-${HOME}/.acc/logs}/migration-0001-build.log"; then
        install_binary "${WORKSPACE}/agent/target/release/acc-agent"
    else
        m_warn "cargo build failed — see ${LOG_DIR:-${HOME}/.acc/logs}/migration-0001-build.log"
    fi
else
    m_warn "No pre-built binary for $(uname -s)-$(uname -m) and cargo not available — skipping"
fi
