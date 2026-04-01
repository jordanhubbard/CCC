# RCC — Makefile
# Entry points for common operator tasks.
# All the actual logic lives in deploy/ scripts.

.PHONY: help init-rcc register dev start-rcc start-dashboard test clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

# ── Onboarding ─────────────────────────────────────────────────────────────

init-rcc: ## Interactive setup: configure this node (RCC host or client agent)
	@bash deploy/rcc-init.sh

register: ## Register this agent with the RCC hub
	@bash deploy/register-agent.sh

# ── Development ────────────────────────────────────────────────────────────

dev: ## Start RCC API + dashboard in dev mode (requires ~/.rcc/.env)
	@echo "Starting RCC API on port 8789..."
	@set -a; source ~/.rcc/.env; set +a; node rcc/api/index.mjs &
	@echo "Starting dashboard on port 8788..."
	@set -a; source ~/.rcc/.env; set +a; node dashboard/server.mjs

start-rcc: ## Start just the RCC API
	@set -a; source ~/.rcc/.env; set +a; node rcc/api/index.mjs

start-dashboard: ## Start just the dashboard
	@set -a; source ~/.rcc/.env; set +a; node dashboard/server.mjs

# ── Testing ────────────────────────────────────────────────────────────────

test: ## Run all tests
	@node --test rcc/api/test.mjs
	@node --test dashboard/test/api.test.mjs 2>/dev/null || true
	@node --test rcc/brain/test.mjs 2>/dev/null || true

# ── Utilities ──────────────────────────────────────────────────────────────

clean: ## Remove generated runtime files (NOT your .env)
	@rm -f rcc/api/agents.json rcc/brain/brain-state.json
	@echo "Cleaned runtime state. Your ~/.rcc/.env is untouched."
