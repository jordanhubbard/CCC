#!/usr/bin/env bash
# push-agentfs-sync.sh — Build agentfs-sync and upload to MinIO shared bin store
#
# Usage: bash deploy/push-agentfs-sync.sh
#
# Builds rcc/agentfs-sync for linux/x86_64, uploads to s3://agents/shared/bin/agentfs-sync.
# Requires: cargo, mc (MinIO client), ~/.rcc/.env with MINIO_* credentials.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_DIR="$REPO_ROOT/rcc/agentfs-sync"
BINARY_NAME="agentfs-sync"
TARGET="x86_64-unknown-linux-musl"
MC_ALIAS="${MINIO_ALIAS:-do-host1}"
DEST_PATH="${MC_ALIAS}/agents/shared/bin/${BINARY_NAME}"

GREEN='\033[0;32m'; BLUE='\033[0;34m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
info()    { echo -e "${BLUE}→${NC} $1"; }
success() { echo -e "${GREEN}✓${NC} $1"; }
warn()    { echo -e "${YELLOW}⚠${NC} $1"; }
error()   { echo -e "${RED}✗${NC} $1"; exit 1; }

# Load .env for MC alias creds
if [[ -f "$HOME/.rcc/.env" ]]; then
  set -a
  # shellcheck source=/dev/null
  source "$HOME/.rcc/.env"
  set +a
fi

# ── Verify source exists ──────────────────────────────────────────────────
if [[ ! -f "$SRC_DIR/Cargo.toml" ]]; then
  error "No Cargo.toml found at $SRC_DIR — agentfs-sync source not yet created.\nCreate rcc/agentfs-sync/ with a Cargo project before running this script."
fi

# ── Ensure target toolchain ───────────────────────────────────────────────
info "Ensuring musl target is installed..."
rustup target add "$TARGET" 2>/dev/null || warn "rustup not found — assuming target already available"

# ── Build ─────────────────────────────────────────────────────────────────
info "Building $BINARY_NAME for $TARGET..."
cargo build --manifest-path "$SRC_DIR/Cargo.toml" \
  --release --target "$TARGET"

BUILT_BIN="$SRC_DIR/target/${TARGET}/release/${BINARY_NAME}"
if [[ ! -f "$BUILT_BIN" ]]; then
  error "Build succeeded but binary not found at $BUILT_BIN"
fi
success "Build complete: $BUILT_BIN ($(du -sh "$BUILT_BIN" | cut -f1))"

# ── Upload via mc ─────────────────────────────────────────────────────────
if ! command -v mc &>/dev/null; then
  error "mc (MinIO client) not found. Install from https://min.io/docs/minio/linux/reference/minio-mc.html"
fi

info "Uploading to ${DEST_PATH}..."
mc cp "$BUILT_BIN" "${DEST_PATH}"
success "agentfs-sync uploaded to MinIO: ${DEST_PATH}"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "agentfs-sync is now at s3://agents/shared/bin/agentfs-sync"
echo ""
echo "To deploy to all agents, run bootstrap on each agent:"
echo "  curl -sSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/deploy/bootstrap.sh | \\"
echo "    bash -s -- --rcc=http://146.190.134.110:8789 --token=<bootstrap-token> --agent=<name>"
echo ""
echo "Or if already bootstrapped, re-run just the agentfs-sync install step:"
echo "  sudo curl -sf http://146.190.134.110:9000/agents/shared/bin/agentfs-sync -o /usr/local/bin/agentfs-sync"
echo "  sudo chmod +x /usr/local/bin/agentfs-sync"
echo "  sudo systemctl restart agentfs-sync"
echo ""
