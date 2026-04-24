#!/usr/bin/env bash
# install-acc.sh — build and install the acc CLI on macOS, Linux, or WSL2.
#
# Usage:
#   bash scripts/install-acc.sh               # build + install to $HOME/.local/bin/acc
#   bash scripts/install-acc.sh --build-only  # build only (skip install)
#   ACC_BIN_DIR=/usr/local/bin bash scripts/install-acc.sh

set -euo pipefail

INSTALL_ONLY=true
BUILD_ONLY=false
for arg in "$@"; do
    case "$arg" in
        --build-only) BUILD_ONLY=true ;;
    esac
done

INSTALL_DIR="${ACC_BIN_DIR:-$HOME/.local/bin}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── Ensure Rust / cargo is available ────────────────────────────────────────

_have_cargo() { command -v cargo >/dev/null 2>&1; }

if ! _have_cargo && [ -f "$HOME/.cargo/env" ]; then
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

if ! _have_cargo; then
    echo "→ Rust not found. Installing via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

echo "→ $(rustc --version)  /  $(cargo --version)"

# ── Build ────────────────────────────────────────────────────────────────────

echo "→ Building acc CLI..."
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" -p acc

# ── Install ──────────────────────────────────────────────────────────────────

ACC_BIN="$REPO_ROOT/target/release/acc"

if [ "$BUILD_ONLY" = "true" ]; then
    echo "✓ Build complete: $ACC_BIN"
    exit 0
fi

mkdir -p "$INSTALL_DIR"
# Atomic replace so a running binary isn't clobbered mid-swap (ETXTBSY)
tmp="$INSTALL_DIR/acc.new.$$"
cp "$ACC_BIN" "$tmp"
mv "$tmp" "$INSTALL_DIR/acc"

echo "✓ Installed: $INSTALL_DIR/acc  ($("$INSTALL_DIR/acc" --version 2>/dev/null || echo "v?"))"

# ── PATH hint ────────────────────────────────────────────────────────────────

if ! command -v acc >/dev/null 2>&1; then
    echo ""
    echo "  acc is not on your PATH. Add this line to your shell profile:"
    echo "    export PATH=\"\$PATH:$INSTALL_DIR\""
fi
