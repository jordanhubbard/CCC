#!/usr/bin/env bash
# bootstrap-wasm-toolchain.sh
# Installs the full Leptos/WASM build toolchain on any agent host.
# Run once. Safe to re-run (idempotent).
#
# Tested on: aarch64/Linux (GPU node), x86_64/Linux (hub), macOS (dev node)

set -euo pipefail

echo "=== SquirrelChat WASM Build Toolchain Bootstrap ==="
echo "Host: $(uname -srm)"

# 1. Ensure rustup is installed
if ! command -v rustup &>/dev/null; then
    echo "→ Installing rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    source "$HOME/.cargo/env"
else
    echo "✓ rustup already installed ($(rustup --version 2>&1 | head -1))"
fi

# Ensure cargo is on PATH
export PATH="$HOME/.cargo/bin:$PATH"

# 2. Ensure stable toolchain
echo "→ Ensuring stable Rust toolchain..."
rustup toolchain install stable --no-self-update
rustup default stable

# 3. Add wasm32 target
echo "→ Adding wasm32-unknown-unknown target..."
rustup target add wasm32-unknown-unknown
echo "✓ wasm32-unknown-unknown target ready"

# 4. Install trunk
if ! command -v trunk &>/dev/null && ! [ -f "$HOME/.cargo/bin/trunk" ]; then
    echo "→ Installing trunk (this may take a few minutes)..."
    cargo install trunk --locked
else
    echo "✓ trunk already installed ($("$HOME/.cargo/bin/trunk" --version 2>/dev/null || trunk --version))"
fi

# 5. Install wasm-bindgen-cli (must match the version in Cargo.lock)
WASM_BINDGEN_VERSION=$(grep -A1 'name = "wasm-bindgen-cli"' \
    "$(dirname "$0")/../Cargo.lock" 2>/dev/null | grep version | sed 's/.*"\(.*\)".*/\1/' || echo "")

if [ -n "$WASM_BINDGEN_VERSION" ]; then
    echo "→ Installing wasm-bindgen-cli $WASM_BINDGEN_VERSION (from Cargo.lock)..."
    cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked 2>/dev/null || \
    cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION"
else
    echo "→ Installing wasm-bindgen-cli (latest)..."
    cargo install wasm-bindgen-cli --locked 2>/dev/null || cargo install wasm-bindgen-cli
fi
echo "✓ wasm-bindgen-cli ready"

# 6. Verify
echo ""
echo "=== Verification ==="
echo "rustc:         $(rustc --version)"
echo "cargo:         $(cargo --version)"
echo "trunk:         $("$HOME/.cargo/bin/trunk" --version 2>/dev/null || trunk --version)"
echo "wasm target:   $(rustup target list --installed | grep wasm32 || echo 'NOT FOUND')"
echo ""
echo "=== Build test ==="
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DASHBOARD_UI="$SCRIPT_DIR/../dashboard-ui"

if [ -d "$DASHBOARD_UI" ]; then
    echo "→ Running trunk build --release in $DASHBOARD_UI ..."
    cd "$DASHBOARD_UI"
    "$HOME/.cargo/bin/trunk" build --release 2>&1 | tail -5
else
    echo "⚠ dashboard-ui not found at $DASHBOARD_UI — skipping build test"
    echo "  Clone the repo and re-run from within it."
fi

echo ""
echo "✅ Bootstrap complete. Any agent can now build the WASM frontend."
echo "   Usage: cd.ccc/dashboard/dashboard-ui && trunk build --release"
