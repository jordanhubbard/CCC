# CCC — Makefile
# Entry points for common operator tasks.
# All the actual logic lives in deploy/ scripts.
#
# OPERATOR QUICK-START (macOS, Linux, WSL2):
#   make deps          # install mc + other operator tools for your platform
#   make env           # create/verify ~/.ccc/.env with hub credentials
#   make sync          # git push + broadcast rcc.update to all agents

.PHONY: help deps deps-check env sync \
        init register build test release clean \
        docker-build docker-up docker-down docker-logs

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

# ── Operator tools (macOS / Linux / WSL2) ─────────────────────────────────
#
# Any machine with internet access and ~/.ccc/.env credentials can control
# a CCC hub. Run `make deps` once to install the required tools.

OS := $(shell uname -s)
IS_WSL := $(shell grep -qi microsoft /proc/version 2>/dev/null && echo true || echo false)

deps: ## Install operator tools (mc, gh) for macOS, Linux, or WSL2
	@echo "Detecting platform..."
ifeq ($(OS),Darwin)
	@echo "→ macOS detected"
	@command -v brew >/dev/null 2>&1 || { echo "ERROR: Homebrew not found. Install from https://brew.sh"; exit 1; }
	@command -v mc   >/dev/null 2>&1 && echo "  mc already installed" || \
		(echo "  Installing mc (MinIO client)..." && brew install minio/stable/mc)
	@command -v gh   >/dev/null 2>&1 && echo "  gh already installed" || \
		(echo "  Installing gh (GitHub CLI)..." && brew install gh)
	@command -v jq   >/dev/null 2>&1 && echo "  jq already installed" || \
		(echo "  Installing jq..." && brew install jq)
else ifeq ($(IS_WSL),true)
	@echo "→ WSL2 detected"
	@$(MAKE) _deps-linux
else ifeq ($(OS),Linux)
	@echo "→ Linux detected"
	@$(MAKE) _deps-linux
else
	@echo "Unknown platform: $(OS). Install mc manually: https://min.io/docs/minio/linux/reference/minio-mc.html"
endif
	@echo ""
	@echo "✓ Operator tools ready. Next: make env"

_deps-linux: ## (internal) Install mc + gh on Linux / WSL2
	@if command -v apt-get >/dev/null 2>&1; then \
		echo "  Using apt (Debian/Ubuntu/WSL2)"; \
		command -v curl >/dev/null 2>&1 || sudo apt-get install -y curl; \
		command -v jq   >/dev/null 2>&1 || sudo apt-get install -y jq; \
		command -v gh   >/dev/null 2>&1 || (type -t gpg >/dev/null 2>&1 || sudo apt-get install -y gpg; \
			curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | sudo dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg; \
			echo "deb [arch=$$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | sudo tee /etc/apt/sources.list.d/github-cli.list; \
			sudo apt-get update -q && sudo apt-get install -y gh); \
	elif command -v dnf >/dev/null 2>&1; then \
		echo "  Using dnf (RHEL/Rocky/Fedora)"; \
		command -v jq >/dev/null 2>&1 || sudo dnf install -y jq; \
		command -v gh >/dev/null 2>&1 || sudo dnf install -y 'dnf-command(config-manager)' && \
			sudo dnf config-manager --add-repo https://cli.github.com/packages/rpm/gh-cli.repo && \
			sudo dnf install -y gh; \
	fi
	@if ! command -v mc >/dev/null 2>&1; then \
		echo "  Installing mc (MinIO client)..."; \
		ARCH=$$(uname -m); \
		case "$$ARCH" in \
			x86_64)  MC_ARCH=linux-amd64 ;; \
			aarch64) MC_ARCH=linux-arm64 ;; \
			*)       MC_ARCH=linux-amd64 ;; \
		esac; \
		curl -sSL "https://dl.min.io/client/mc/release/$$MC_ARCH/mc" -o /tmp/mc-bin; \
		sudo install -m 755 /tmp/mc-bin /usr/local/bin/mc; \
		rm -f /tmp/mc-bin; \
		echo "  mc installed to /usr/local/bin/mc"; \
	else \
		echo "  mc already installed"; \
	fi

deps-check: ## Verify all operator tools are installed
	@echo "Checking operator tools..."
	@command -v mc   >/dev/null 2>&1 && echo "  ✓ mc   (MinIO client)" || echo "  ✗ mc   — run: make deps"
	@command -v git  >/dev/null 2>&1 && echo "  ✓ git" || echo "  ✗ git  — install git"
	@command -v curl >/dev/null 2>&1 && echo "  ✓ curl" || echo "  ✗ curl — install curl"
	@command -v jq   >/dev/null 2>&1 && echo "  ✓ jq" || echo "  ✗ jq   — run: make deps"
	@command -v gh   >/dev/null 2>&1 && echo "  ✓ gh   (GitHub CLI)" || echo "  ✗ gh   — run: make deps"
	@echo ""
	@if [ -f "$$HOME/.ccc/.env" ]; then \
		echo "  ✓ ~/.ccc/.env present"; \
		grep -q "^CCC_URL=" "$$HOME/.ccc/.env"         && echo "  ✓ CCC_URL set"         || echo "  ✗ CCC_URL missing in ~/.ccc/.env"; \
		grep -q "^CCC_AGENT_TOKEN=" "$$HOME/.ccc/.env" && echo "  ✓ CCC_AGENT_TOKEN set" || echo "  ✗ CCC_AGENT_TOKEN missing in ~/.ccc/.env"; \
	else \
		echo "  ✗ ~/.ccc/.env missing — run: make env"; \
	fi

env: ## Create or verify ~/.ccc/.env (prompts for missing values)
	@mkdir -p "$$HOME/.ccc"
	@if [ ! -f "$$HOME/.ccc/.env" ]; then \
		cp deploy/.env.template "$$HOME/.ccc/.env"; \
		chmod 600 "$$HOME/.ccc/.env"; \
		echo "Created ~/.ccc/.env from template. Edit it to set CCC_URL and CCC_AGENT_TOKEN."; \
		echo "  $$EDITOR ~/.ccc/.env"; \
	else \
		echo "~/.ccc/.env already exists."; \
		$(MAKE) deps-check; \
	fi

# ── Fleet sync ─────────────────────────────────────────────────────────────

sync: ## Push local changes to GitHub and broadcast rcc.update to all agents
	@git push
	@bash deploy/fleet-sync.sh

# ── Onboarding ─────────────────────────────────────────────────────────────

init: ## Interactive setup: configure this node
	@bash deploy/ccc-init.sh

register: ## Register this agent with the CCC hub
	@bash deploy/register-agent.sh

# ── Build ──────────────────────────────────────────────────────────────────

build: ## Build all Rust binaries (ccc-server, ccc-agent)
	@cargo build --release --manifest-path Cargo.toml

# ── Testing ────────────────────────────────────────────────────────────────

test: ## Run all Rust tests
	@cargo test --manifest-path Cargo.toml

# ── Release ────────────────────────────────────────────────────────────────

release: ## Cut a release: bump patch version, update CHANGELOG, tag, push, GH release
	@bash scripts/release.sh patch

release-minor: ## Cut a minor release
	@bash scripts/release.sh minor

release-major: ## Cut a major release
	@bash scripts/release.sh major

# ── Docker (Operator path) ─────────────────────────────────────────────────

docker-build: ## Build the CCC Docker image locally
	docker build -t ccc:local .

docker-up: ## Start the full CCC stack via Docker Compose
	docker compose up -d

docker-down: ## Stop the CCC Docker stack
	docker compose down

docker-logs: ## Tail logs from all CCC containers
	docker compose logs -f

# ── Utilities ──────────────────────────────────────────────────────────────

clean: ## Remove build artifacts
	@cargo clean --manifest-path Cargo.toml
	@echo "Cleaned build artifacts."
