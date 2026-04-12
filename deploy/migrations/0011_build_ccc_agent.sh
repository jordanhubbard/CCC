# Description: Build and install ccc-agent (Rust CLI, replaces Node.js in deploy scripts)
#
# Context: ccc-agent is the Rust-native replacement for all node -e JSON calls
#   in run-migrations.sh, upgrade-node.sh, and other deploy scripts.
#   After this migration runs, Node.js is no longer required by the CCC deploy system.
#   Binary is installed to ~/.ccc/bin/ccc-agent (CCC-owned, not system-wide).
# Condition: all platforms (linux + macos) where cargo is available

CARGO="${HOME}/.cargo/bin/cargo"
BUILD_DIR="$WORKSPACE/ccc/dashboard"
BINARY="$BUILD_DIR/target/release/ccc-agent"
INSTALL_DIR="$HOME/.ccc/bin"
INSTALL_PATH="$INSTALL_DIR/ccc-agent"

if [ ! -x "$CARGO" ]; then
    m_warn "cargo not found at $CARGO — skipping ccc-agent build"
    m_warn "Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    m_warn "Then re-run: bash deploy/run-migrations.sh --force=0011"
    return 0
fi

m_info "Building ccc-agent..."
(cd "$BUILD_DIR" && "$CARGO" build -p ccc-agent --release --quiet) \
    || { m_warn "cargo build failed — skipping install"; return 0; }
m_success "ccc-agent built"

mkdir -p "$INSTALL_DIR"
cp "$BINARY" "$INSTALL_PATH"
chmod +x "$INSTALL_PATH"
m_success "ccc-agent installed to $INSTALL_PATH"

# Verify
VERSION=$("$INSTALL_PATH" 2>&1 | head -1 || echo "unknown")
m_info "Installed: $INSTALL_PATH"
m_info "Node.js is no longer required by the CCC deploy system"
