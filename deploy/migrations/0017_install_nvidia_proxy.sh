#!/usr/bin/env bash
# Description: Install acc-nvidia-proxy — header-stripping proxy for NVIDIA LiteLLM.
#
# Claude CLI sends anthropic-beta headers that NVIDIA's LiteLLM proxy rejects
# with "invalid beta flag". This migration installs a local Python proxy on
# port 9099 that strips those headers before forwarding to NVIDIA, then
# updates ANTHROPIC_BASE_URL in ~/.acc/.env to point to localhost:9099.
#
# Safe to run multiple times. All operations are idempotent.

set -euo pipefail

ACC_DEST="${HOME}/.acc"
[[ -d "$ACC_DEST" ]] || ACC_DEST="${HOME}/.ccc"
ENV_FILE="${ACC_DEST}/.env"
WORKSPACE="${ACC_DEST}/workspace"
PROXY_SCRIPT="${WORKSPACE}/deploy/nvidia-proxy.py"

m_info "Install acc-nvidia-proxy (NVIDIA LiteLLM anthropic-beta compat)"

# ── Step 1: Verify proxy script exists ───────────────────────────────────────
if [[ ! -f "$PROXY_SCRIPT" ]]; then
  m_warn "nvidia-proxy.py not found at ${PROXY_SCRIPT} — pull latest workspace first"
  exit 1
fi
m_success "nvidia-proxy.py found"

# ── Step 2: Update ANTHROPIC_BASE_URL in .env ────────────────────────────────
if [[ -f "$ENV_FILE" ]]; then
  if grep -q "^ANTHROPIC_BASE_URL=http://localhost:9099" "$ENV_FILE" 2>/dev/null; then
    m_skip "ANTHROPIC_BASE_URL already set to localhost:9099 in ${ENV_FILE}"
  else
    # Replace or append ANTHROPIC_BASE_URL
    if grep -q "^ANTHROPIC_BASE_URL=" "$ENV_FILE" 2>/dev/null; then
      sed -i.bak "s|^ANTHROPIC_BASE_URL=.*|ANTHROPIC_BASE_URL=http://localhost:9099|g" \
        "$ENV_FILE" && rm -f "${ENV_FILE}.bak"
      m_success "Updated ANTHROPIC_BASE_URL → http://localhost:9099 in ${ENV_FILE}"
    else
      echo "ANTHROPIC_BASE_URL=http://localhost:9099" >> "$ENV_FILE"
      m_success "Added ANTHROPIC_BASE_URL=http://localhost:9099 to ${ENV_FILE}"
    fi
  fi
else
  m_warn "No .env found at ${ENV_FILE} — skipping ANTHROPIC_BASE_URL update"
fi

# ── Step 3: Install the service ───────────────────────────────────────────────
if on_platform linux; then
  if [[ -f "${WORKSPACE}/deploy/install-queue-worker.sh" ]]; then
    # install-queue-worker.sh now calls _install_nvidia_proxy internally
    bash "${WORKSPACE}/deploy/install-queue-worker.sh" linux \
      && m_success "acc-nvidia-proxy installed (via install-queue-worker.sh)" \
      || m_warn "install-queue-worker.sh returned non-zero — check systemd status"
  else
    # Manual install fallback
    SVC_FILE="/etc/systemd/system/acc-nvidia-proxy.service"
    TMPL="${WORKSPACE}/deploy/systemd/acc-nvidia-proxy.service"
    if [[ -f "$TMPL" ]]; then
      sed "s|AGENT_USER|${USER}|g; s|AGENT_HOME|${HOME}|g" "$TMPL" \
        | sudo tee "$SVC_FILE" > /dev/null
      sudo systemctl daemon-reload
      sudo systemctl enable acc-nvidia-proxy
      sudo systemctl restart acc-nvidia-proxy
      m_success "acc-nvidia-proxy.service installed and started"
    else
      m_warn "systemd template not found — install manually"
    fi
  fi
fi

if on_platform macos; then
  PLIST_DST="${HOME}/Library/LaunchAgents/com.acc.nvidia-proxy.plist"
  TMPL="${WORKSPACE}/deploy/launchd/com.acc.nvidia-proxy.plist"
  if [[ -f "$TMPL" ]]; then
    mkdir -p "${HOME}/Library/LaunchAgents"
    sed "s|AGENT_USER|${USER}|g; s|AGENT_HOME|${HOME}|g" "$TMPL" > "$PLIST_DST"
    launchctl unload "$PLIST_DST" 2>/dev/null || true
    launchctl load -w "$PLIST_DST"
    m_success "com.acc.nvidia-proxy.plist loaded"
  else
    m_warn "launchd plist template not found"
  fi
fi

m_success "Migration 0017 complete — acc-nvidia-proxy running on localhost:9099"
m_info "Claude CLI will now route inference through the local proxy"
m_info "ANTHROPIC_BASE_URL=http://localhost:9099 in ${ENV_FILE}"
