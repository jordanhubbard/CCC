#!/usr/bin/env bash
# Description: Rebuild acc-agent after repo-wide Cargo workspace conversion
#
# Context: Commit ce5e810 promoted acc-cli/acc-server/agent/acc-agent into a
# single Cargo workspace at repo root. As a result:
#   - agent/Cargo.toml no longer exists (was the inner workspace)
#   - Cargo's target directory moved from agent/target/ to target/ at repo root
#
# Migration 0001's cargo fallback path (`--manifest-path ${WORKSPACE}/agent/Cargo.toml`)
# therefore fails on any node that needs to build from source (primarily non-linux-amd64
# platforms that can't use the pre-built acc-agent-linux-amd64). 0001 itself is not
# edited — it already ran or will run unchanged — but nodes whose cargo fallback
# failed (or which were never served a pre-built binary) need this follow-up to
# get a current acc-agent installed.
#
# Idempotent: skips the rebuild if the installed binary's mtime is newer than
# the workspace's Cargo.lock (i.e., the install is already in sync with the tree).
# Condition: all agent nodes (linux + macos)

DEST_BIN="${ACC_DIR:-${HOME}/.acc}/bin/acc-agent"
WORKSPACE="${WORKSPACE:-${HOME}/.acc/workspace}"

# Pick the right pre-built binary (mirrors 0001's platform matrix)
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)   SRC="${WORKSPACE}/acc-agent-linux-amd64" ;;
    Darwin-arm64)   SRC="${WORKSPACE}/acc-agent" ;;
    Darwin-x86_64)  SRC="${WORKSPACE}/acc-agent" ;;
    *)              SRC="${WORKSPACE}/acc-agent" ;;
esac

install_binary() {
    local src="$1"
    mkdir -p "$(dirname "${DEST_BIN}")"
    local tmp="${DEST_BIN}.new.$$"
    cp "${src}" "${tmp}"
    chmod +x "${tmp}"
    mv "${tmp}" "${DEST_BIN}"
    m_success "Installed $(basename "${src}") → ${DEST_BIN}"
}

# Idempotency: skip if installed binary is newer than Cargo.lock
LOCK="${WORKSPACE}/Cargo.lock"
if [ -f "${DEST_BIN}" ] && [ -f "${LOCK}" ] && [ "${DEST_BIN}" -nt "${LOCK}" ]; then
    m_skip "acc-agent already current (binary newer than Cargo.lock)"
elif [ "${DRY_RUN:-false}" = "true" ]; then
    m_info "[dry-run] would rebuild/install acc-agent → ${DEST_BIN}"
elif [ -f "${SRC}" ] && file "${SRC}" | grep -q "$(uname -m)"; then
    install_binary "${SRC}"
elif command -v cargo &>/dev/null && [ -f "${WORKSPACE}/Cargo.toml" ]; then
    m_info "Building acc-agent from workspace source (this takes ~2min)"
    export PATH="${HOME}/.cargo/bin:${PATH}"
    LOG="${LOG_DIR:-${HOME}/.acc/logs}/migration-0002-build.log"
    mkdir -p "$(dirname "${LOG}")"
    if cargo build --release --manifest-path "${WORKSPACE}/Cargo.toml" \
            -p acc-agent --quiet 2>>"${LOG}"; then
        install_binary "${WORKSPACE}/target/release/acc-agent"
    else
        m_warn "cargo build failed — see ${LOG}"
    fi
else
    m_warn "No pre-built binary for $(uname -s)-$(uname -m) and cargo not available — skipping"
fi
