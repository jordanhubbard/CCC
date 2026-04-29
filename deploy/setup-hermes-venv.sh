#!/usr/bin/env bash
# DEPRECATED: Python hermes-agent has been replaced by the native Rust
# acc-agent runtime. This stub is intentionally kept so old runbooks fail
# with a clear migration message instead of installing a stale `hermes`
# command that competes with `acc-agent hermes --gateway`.
set -euo pipefail

echo "[hermes-venv] ERROR: Python hermes-agent is retired." >&2
echo "[hermes-venv] Use the native runtime instead:" >&2
echo "  ~/.acc/bin/acc-agent hermes --chat" >&2
echo "  ~/.acc/bin/acc-agent hermes --query \"hello\"" >&2
echo "  ~/.acc/bin/acc-agent hermes --gateway" >&2
echo "  bash ~/Src/ACC/deploy/restart-agent.sh" >&2
exit 1
