#!/usr/bin/env bash
# rebuild-changed.sh — Rebuild Rust binaries when their source directories changed.
#
# Called by upgrade-node.sh after git pull, with:
#   BEFORE_SHA  — git SHA before the pull (may equal AFTER_SHA if nothing changed)
#   AFTER_SHA   — git SHA after the pull (current HEAD)
#   WORKSPACE   — path to the git workspace
#   ACC_DIR     — path to the acc data dir (~/.acc)
#   IS_HUB      — "true" if this node runs acc-server (from .env)
#   DRY_RUN     — "true" to skip actual builds
#
# What it rebuilds:
#   agent/         → acc-agent  → $ACC_DIR/bin/acc-agent   (all nodes)
#   acc-server/    → acc-server → /usr/local/bin/acc-server (hub only)

set -euo pipefail

BEFORE_SHA="${BEFORE_SHA:-}"
AFTER_SHA="${AFTER_SHA:-$(git -C "${WORKSPACE}" rev-parse HEAD 2>/dev/null || echo "")}"
IS_HUB="${IS_HUB:-false}"
DRY_RUN="${DRY_RUN:-false}"
LOG_DIR="${LOG_DIR:-${ACC_DIR:-$HOME/.acc}/logs}"

# Colors (safe to use even if not exported — declare inline)
BLUE='\033[0;34m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
rb_info()    { echo -e "${BLUE}  [rebuild]${NC} $1"; }
rb_success() { echo -e "${GREEN}  [rebuild]${NC} ✓ $1"; }
rb_warn()    { echo -e "${YELLOW}  [rebuild]${NC} ⚠ $1"; }
rb_skip()    { echo -e "${BLUE}  [rebuild]${NC} → $1 (unchanged)"; }

# Nothing to compare — first run or no pull happened
if [ -z "${BEFORE_SHA}" ] || [ "${BEFORE_SHA}" = "${AFTER_SHA}" ]; then
  rb_info "No new commits — skipping source rebuild"
  exit 0
fi

changed_dirs() {
  git -C "${WORKSPACE}" diff --name-only "${BEFORE_SHA}" "${AFTER_SHA}" 2>/dev/null \
    | awk -F/ '{print $1"/"$2}' | sort -u
}

CHANGED="$(changed_dirs)"

# ── Cargo setup ───────────────────────────────────────────────────────────
export PATH="${HOME}/.cargo/bin:${PATH}"
CARGO="$(command -v cargo 2>/dev/null || echo "")"

build_and_install() {
  local manifest="$1" out_bin="$2" dest="$3" label="$4" restart_cmd="$5"

  if [ -z "${CARGO}" ]; then
    rb_warn "cargo not found — cannot rebuild ${label}"
    return
  fi

  if [ "${DRY_RUN}" = "true" ]; then
    rb_info "[dry-run] would build ${label} and install to ${dest}"
    return
  fi

  rb_info "Building ${label} from source (this may take a few minutes)…"
  local log="${LOG_DIR}/rebuild-${label}.log"
  mkdir -p "${LOG_DIR}"

  if cargo build --release --manifest-path "${manifest}" --quiet 2>>"${log}"; then
    mkdir -p "$(dirname "${dest}")"
    local tmp="${dest}.new.$$"
    cp "${out_bin}" "${tmp}"
    chmod +x "${tmp}"
    mv "${tmp}" "${dest}"
    rb_success "${label} installed → ${dest}"

    # Restart service if a restart command was provided
    if [ -n "${restart_cmd}" ]; then
      eval "${restart_cmd}" || rb_warn "restart of ${label} failed (non-fatal)"
    fi
  else
    rb_warn "${label} build failed — see ${log}"
  fi
}

# ── acc-agent (all nodes) ──────────────────────────────────────────────────
if echo "${CHANGED}" | grep -q "^agent/"; then
  AGENT_DEST="${ACC_DIR:-$HOME/.acc}/bin/acc-agent"
  AGENT_MANIFEST="${WORKSPACE}/agent/acc-agent/Cargo.toml"
  AGENT_BIN="${WORKSPACE}/target/release/acc-agent"

  # Prefer pre-built binary if it's newer than the last build
  if [ -f "${WORKSPACE}/acc-agent" ] && \
     [ "${WORKSPACE}/acc-agent" -nt "${AGENT_DEST}" ] 2>/dev/null; then
    if [ "${DRY_RUN}" = "true" ]; then
      rb_info "[dry-run] would install pre-built acc-agent from workspace"
    else
      tmp="${AGENT_DEST}.new.$$"
      cp "${WORKSPACE}/acc-agent" "${tmp}" && chmod +x "${tmp}" && mv "${tmp}" "${AGENT_DEST}"
      rb_success "acc-agent installed from pre-built workspace binary → ${AGENT_DEST}"
    fi
  elif [ -f "${AGENT_MANIFEST}" ]; then
    # Determine restart: kill acc-agent so the supervisor or timer relaunches it
    restart="pkill -x acc-agent 2>/dev/null || true"
    build_and_install "${AGENT_MANIFEST}" "${AGENT_BIN}" "${AGENT_DEST}" "acc-agent" "${restart}"
  else
    rb_warn "agent/acc-agent/Cargo.toml not found — cannot rebuild acc-agent"
  fi
else
  rb_skip "acc-agent"
fi

# ── acc-server (hub only) ──────────────────────────────────────────────────
if [ "${IS_HUB}" = "true" ] && echo "${CHANGED}" | grep -q "^acc-server/"; then
  SERVER_MANIFEST="${WORKSPACE}/acc-server/Cargo.toml"
  SERVER_BIN="${WORKSPACE}/target/release/acc-server"
  SERVER_DEST="/usr/local/bin/acc-server"

  if [ -f "${SERVER_MANIFEST}" ]; then
    # Restart: replace binary then SIGTERM old process (Axum graceful shutdown)
    restart="
      sudo cp \"${SERVER_BIN}\" \"${SERVER_DEST}.new\" && \
      sudo mv \"${SERVER_DEST}.new\" \"${SERVER_DEST}\" && \
      pkill -f /usr/local/bin/acc-server || true; sleep 1; \
      ( cd / && nohup ${SERVER_DEST} >> \"${LOG_DIR}/acc-server.log\" 2>&1 & disown || true )
    "
    build_and_install "${SERVER_MANIFEST}" "${SERVER_BIN}" "${SERVER_DEST}" "acc-server" "${restart}"
  else
    rb_warn "acc-server/Cargo.toml not found — cannot rebuild acc-server"
  fi
elif [ "${IS_HUB}" = "true" ]; then
  rb_skip "acc-server"
fi
