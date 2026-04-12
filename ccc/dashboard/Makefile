.PHONY: build dev release clean check install-tools

# Paths
CLAWCHAT_UI   := ../clawchat
CCC_SERVER    := ccc-server
DIST          := $(CLAWCHAT_UI)/dist

# ── Tool availability checks ──────────────────────────────────────────────
check:
	@echo "=== Build tool check ==="
	@command -v cargo >/dev/null 2>&1 && echo "  ✓ cargo" || echo "  ✗ cargo (install Rust via rustup)"
	@command -v trunk >/dev/null 2>&1  && echo "  ✓ trunk" || echo "  ✗ trunk (run: cargo install trunk)"
	@rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown \
		&& echo "  ✓ wasm32 target" \
		|| echo "  ✗ wasm32 target (run: rustup target add wasm32-unknown-unknown)"
	@command -v wasm-opt >/dev/null 2>&1 && echo "  ✓ wasm-opt" || echo "  ~ wasm-opt (optional, for smaller WASM)"

install-tools:
	rustup target add wasm32-unknown-unknown
	cargo install trunk

# ── Primary: ccc-server + ClawChat ───────────────────────────────────────
# ccc-server serves both the API and the WASM SPA when DASHBOARD_DIST is set.
# This replaces the old dashboard-server proxy pattern.

build: build-ui build-server

build-ui:
	@echo "=== Building ClawChat WASM ==="
	cd $(CLAWCHAT_UI) && trunk build

build-server:
	@echo "=== Building ccc-server ==="
	cargo build -p ccc-server

# ── Release ───────────────────────────────────────────────────────────────
release: release-ui release-server

release-ui:
	@echo "=== Building ClawChat WASM (release) ==="
	cd $(CLAWCHAT_UI) && trunk build --release

release-server:
	@echo "=== Building ccc-server (release) ==="
	cargo build -p ccc-server --release

# ── Dev (hot-reload ClawChat + ccc-server) ────────────────────────────────
# trunk serves ClawChat dev at :8080 and proxies /bus/* to ccc-server.
# ccc-server serves the API at :8789.
dev:
	@echo "Starting dev: trunk watch + ccc-server"
	@echo "  ClawChat dev: http://localhost:8080"
	@echo "  API: http://localhost:8789"
	@trap 'kill %1 %2 2>/dev/null' EXIT; \
		cd $(CLAWCHAT_UI) && trunk watch & \
		cargo run -p ccc-server & \
		wait

# ── Run (after build) ─────────────────────────────────────────────────────
run: build-server
	DASHBOARD_DIST=$(DIST) \
	CCC_PORT=$${CCC_PORT:-8789} \
	CCC_AUTH_TOKENS=$${CCC_AUTH_TOKENS:-} \
	./target/debug/ccc-server

run-release: release-server
	DASHBOARD_DIST=$(DIST) \
	CCC_PORT=$${CCC_PORT:-8789} \
	CCC_AUTH_TOKENS=$${CCC_AUTH_TOKENS:-} \
	./target/release/ccc-server

# ── Clean ─────────────────────────────────────────────────────────────────
clean:
	cargo clean
	rm -rf $(DIST)

