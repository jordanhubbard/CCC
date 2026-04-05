#!/usr/bin/env bash
# install-sbom.sh — Idempotent SBOM enforcer for CCC agent nodes
# Usage: install-sbom.sh [path/to/sbom.json]
# If no path given, looks for ~/.rcc/sbom.json or rcc/sbom/<AGENT_NAME>.json

set -euo pipefail

SBOM_PATH="${1:-}"
AGENT_NAME="${AGENT_NAME:-$(hostname -s)}"
CCC_URL="${CCC_URL:-http://localhost:8789}"
CCC_AGENT_TOKEN="${CCC_AGENT_TOKEN:-}"

# ── Find SBOM ──────────────────────────────────────────────────────────────
if [ -z "$SBOM_PATH" ]; then
  if [ -f "$HOME/.rcc/sbom.json" ]; then
    SBOM_PATH="$HOME/.rcc/sbom.json"
  elif [ -f "$(dirname "$0")/${AGENT_NAME}.json" ]; then
    SBOM_PATH="$(dirname "$0")/${AGENT_NAME}.json"
  else
    echo "ERROR: No SBOM found. Pass path as argument or place at ~/.rcc/sbom.json"
    exit 1
  fi
fi

echo "🔍 Loading SBOM from: $SBOM_PATH"
if ! command -v jq &>/dev/null; then
  echo "ERROR: jq required to parse SBOM. Install it first: apt install jq / brew install jq"
  exit 1
fi

OS=$(jq -r '.os // "linux"' "$SBOM_PATH")
echo "📋 SBOM for agent: $(jq -r '.agent' "$SBOM_PATH") (os: $OS)"

# ── Package install helpers ────────────────────────────────────────────────
install_apt() {
  local pkgs=("$@")
  if [ ${#pkgs[@]} -eq 0 ]; then return; fi
  echo "📦 Installing apt packages: ${pkgs[*]}"
  if command -v apt-get &>/dev/null; then
    sudo apt-get install -y "${pkgs[@]}" 2>/dev/null || echo "  ⚠️  Some apt packages may have failed (non-fatal)"
  else
    echo "  ℹ️  apt not available on this system — skipping"
  fi
}

install_brew() {
  local pkgs=("$@")
  if [ ${#pkgs[@]} -eq 0 ]; then return; fi
  echo "🍺 Installing brew packages: ${pkgs[*]}"
  if command -v brew &>/dev/null; then
    brew install "${pkgs[@]}" 2>/dev/null || echo "  ⚠️  Some brew packages may have failed (non-fatal)"
  else
    echo "  ℹ️  brew not available — skipping"
  fi
}

install_npm() {
  local pkgs=("$@")
  if [ ${#pkgs[@]} -eq 0 ]; then return; fi
  echo "📦 Installing npm global packages: ${pkgs[*]}"
  if command -v npm &>/dev/null; then
    npm install -g "${pkgs[@]}" 2>/dev/null || echo "  ⚠️  Some npm installs may have failed (non-fatal)"
  else
    echo "  ℹ️  npm not available — skipping"
  fi
}

install_pip() {
  local pkgs=("$@")
  if [ ${#pkgs[@]} -eq 0 ]; then return; fi
  echo "🐍 Installing pip packages: ${pkgs[*]}"
  if command -v pip3 &>/dev/null; then
    pip3 install --quiet "${pkgs[@]}" 2>/dev/null || echo "  ⚠️  Some pip installs may have failed (non-fatal)"
  else
    echo "  ℹ️  pip3 not available — skipping"
  fi
}

# ── Extract and install packages ───────────────────────────────────────────
APT_PKGS=$(jq -r '.packages.apt // [] | .[]' "$SBOM_PATH" | tr '\n' ' ')
BREW_PKGS=$(jq -r '.packages.brew // [] | .[]' "$SBOM_PATH" | tr '\n' ' ')
NPM_PKGS=$(jq -r '.packages.npm // [] | .[]' "$SBOM_PATH" | tr '\n' ' ')
PIP_PKGS=$(jq -r '.packages.pip // [] | .[]' "$SBOM_PATH" | tr '\n' ' ')

[ -n "$APT_PKGS" ] && install_apt $APT_PKGS
[ -n "$BREW_PKGS" ] && install_brew $BREW_PKGS
[ -n "$NPM_PKGS" ] && install_npm $NPM_PKGS
[ -n "$PIP_PKGS" ] && install_pip $PIP_PKGS

# ── Check tool versions ────────────────────────────────────────────────────
echo ""
echo "🔧 Checking required tools..."
TOOLS=$(jq -r '.tools // {} | keys[]' "$SBOM_PATH" 2>/dev/null || echo "")
MISSING=()
for tool in $TOOLS; do
  if command -v "$tool" &>/dev/null; then
    echo "  ✅ $tool: $(command -v "$tool")"
  else
    echo "  ❌ $tool: NOT FOUND"
    MISSING+=("$tool")
  fi
done
if [ ${#MISSING[@]} -gt 0 ]; then
  echo ""
  echo "⚠️  Missing tools: ${MISSING[*]}"
  echo "   These may need manual installation."
fi

# ── Check env vars ─────────────────────────────────────────────────────────
echo ""
echo "🔑 Checking required environment variables..."
REQUIRED_ENV=$(jq -r '.env_required // [] | .[]' "$SBOM_PATH" 2>/dev/null || echo "")
ENV_MISSING=()
for var in $REQUIRED_ENV; do
  if [ -n "${!var:-}" ]; then
    echo "  ✅ $var: set"
  else
    echo "  ❌ $var: NOT SET"
    ENV_MISSING+=("$var")
  fi
done
if [ ${#ENV_MISSING[@]} -gt 0 ]; then
  echo ""
  echo "⚠️  Missing required env vars: ${ENV_MISSING[*]}"
  echo "   Add them to ~/.rcc/.env and source it."
fi

# ── Post SBOM to hub ───────────────────────────────────────────────────────
if [ -n "$CCC_AGENT_TOKEN" ] && [ -n "$CCC_URL" ]; then
  echo ""
  echo "📡 Syncing SBOM to hub at $CCC_URL..."
  HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$CCC_URL/api/sbom/$AGENT_NAME" \
    -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
    -H "Content-Type: application/json" \
    -d @"$SBOM_PATH" 2>/dev/null || echo "000")
  if [ "$HTTP_CODE" = "200" ] || [ "$HTTP_CODE" = "201" ]; then
    echo "  ✅ SBOM synced to hub"
  else
    echo "  ⚠️  SBOM sync returned HTTP $HTTP_CODE (hub may not support SBOM yet)"
  fi
fi

echo ""
echo "✅ SBOM enforcement complete for $(jq -r '.agent' "$SBOM_PATH")"
