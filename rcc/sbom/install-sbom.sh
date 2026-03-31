#!/usr/bin/env bash
# install-sbom.sh — Idempotent SBOM installer for RCC agent nodes
# Usage: AGENT_NAME=rocky bash install-sbom.sh [/path/to/sbom.json]
# Or:    bash install-sbom.sh rocky.sbom.json

set -euo pipefail

SBOM_FILE="${1:-}"
AGENT_NAME="${AGENT_NAME:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RCC_URL="${RCC_URL:-http://localhost:8789}"

# Detect platform
PLATFORM="linux"
if [[ "$(uname)" == "Darwin" ]]; then
  PLATFORM="macos"
fi

# Resolve SBOM file
if [[ -z "$SBOM_FILE" ]]; then
  if [[ -n "$AGENT_NAME" ]]; then
    SBOM_FILE="$SCRIPT_DIR/${AGENT_NAME}.sbom.json"
  else
    echo "❌ Usage: AGENT_NAME=<name> bash install-sbom.sh  OR  bash install-sbom.sh <file.sbom.json>"
    exit 1
  fi
fi

if [[ ! -f "$SBOM_FILE" ]]; then
  # Try fetching from RCC hub
  if [[ -n "$AGENT_NAME" && -n "$RCC_URL" ]]; then
    echo "📥 Fetching SBOM from $RCC_URL/api/sbom/$AGENT_NAME ..."
    curl -fsSL "$RCC_URL/api/sbom/$AGENT_NAME" -o "/tmp/${AGENT_NAME}.sbom.json" 2>/dev/null || {
      echo "❌ SBOM file not found: $SBOM_FILE and could not fetch from hub"
      exit 1
    }
    SBOM_FILE="/tmp/${AGENT_NAME}.sbom.json"
  else
    echo "❌ SBOM file not found: $SBOM_FILE"
    exit 1
  fi
fi

echo "🔧 Installing SBOM: $SBOM_FILE"

# Parse SBOM with node (available on all agent nodes)
if ! command -v node &>/dev/null; then
  echo "❌ node is required to parse SBOM JSON. Install Node.js first."
  exit 1
fi

# Extract fields using node
read_field() {
  node -e "const s=require('$SBOM_FILE'); const v=s.$1; console.log(Array.isArray(v)?v.join('\n'):(v||''));" 2>/dev/null || echo ""
}

read_obj_keys() {
  node -e "const s=require('$SBOM_FILE'); const v=s.$1; if(v)Object.keys(v).forEach(k=>console.log(k));" 2>/dev/null || true
}

read_pkg_list() {
  node -e "const s=require('$SBOM_FILE'); const v=(s.packages||{})['$1']||[]; v.forEach(x=>console.log(x));" 2>/dev/null || true
}

AGENT=$(read_field agent)
SBOM_PLATFORM=$(read_field platform)
echo "  Agent: $AGENT"
echo "  Platform: $SBOM_PLATFORM (running on: $PLATFORM)"

# ── APT packages ─────────────────────────────────────────────────────────────
APT_PKGS=$(read_pkg_list apt)
if [[ -n "$APT_PKGS" && "$PLATFORM" == "linux" ]]; then
  echo ""
  echo "📦 APT packages..."
  MISSING_APT=()
  while IFS= read -r pkg; do
    [[ -z "$pkg" ]] && continue
    if ! dpkg -l "$pkg" &>/dev/null 2>&1; then
      MISSING_APT+=("$pkg")
    else
      echo "  ✓ $pkg"
    fi
  done <<< "$APT_PKGS"

  if [[ ${#MISSING_APT[@]} -gt 0 ]]; then
    echo "  Installing: ${MISSING_APT[*]}"
    sudo apt-get install -y "${MISSING_APT[@]}" 2>/dev/null || apt-get install -y "${MISSING_APT[@]}"
  fi
fi

# ── Homebrew packages ─────────────────────────────────────────────────────────
BREW_PKGS=$(read_pkg_list brew)
if [[ -n "$BREW_PKGS" && "$PLATFORM" == "macos" ]]; then
  echo ""
  echo "🍺 Homebrew packages..."
  if command -v brew &>/dev/null; then
    while IFS= read -r pkg; do
      [[ -z "$pkg" ]] && continue
      if brew list "$pkg" &>/dev/null 2>&1; then
        echo "  ✓ $pkg"
      else
        echo "  Installing: $pkg"
        brew install "$pkg"
      fi
    done <<< "$BREW_PKGS"
  else
    echo "  ⚠️  brew not found, skipping Homebrew packages"
  fi
fi

# ── NPM packages ─────────────────────────────────────────────────────────────
NPM_PKGS=$(read_pkg_list npm)
if [[ -n "$NPM_PKGS" ]]; then
  echo ""
  echo "📦 NPM packages..."
  while IFS= read -r pkg; do
    [[ -z "$pkg" ]] && continue
    # Check for global flag
    if [[ "$pkg" == -g* ]]; then
      PKG_NAME="${pkg#-g }"
      PKG_NAME="${pkg#-g}"
      PKG_NAME="${PKG_NAME# }"
      if npm list -g "$PKG_NAME" &>/dev/null 2>&1; then
        echo "  ✓ $PKG_NAME (global)"
      else
        echo "  Installing globally: $PKG_NAME"
        npm install -g "$PKG_NAME"
      fi
    else
      # Local package — just note it (no install location context)
      echo "  ℹ️  Local npm package '$pkg' — install manually in project dir"
    fi
  done <<< "$NPM_PKGS"
fi

# ── PIP packages ─────────────────────────────────────────────────────────────
PIP_PKGS=$(read_pkg_list pip)
if [[ -n "$PIP_PKGS" ]]; then
  echo ""
  echo "🐍 Python packages..."
  PIP_CMD="pip3"
  command -v pip3 &>/dev/null || PIP_CMD="pip"
  
  while IFS= read -r pkg; do
    [[ -z "$pkg" ]] && continue
    if $PIP_CMD show "$pkg" &>/dev/null 2>&1; then
      echo "  ✓ $pkg"
    else
      echo "  Installing: $pkg"
      $PIP_CMD install --quiet "$pkg"
    fi
  done <<< "$PIP_PKGS"
fi

# ── Skills ────────────────────────────────────────────────────────────────────
SKILLS=$(read_field skills)
if [[ -n "$SKILLS" ]]; then
  echo ""
  echo "🎭 OpenClaw skills..."
  SKILLS_DIR="${OPENCLAW_SKILLS_DIR:-$HOME/.openclaw/workspace/skills}"
  CLAWHUB_CMD=""
  command -v clawhub &>/dev/null && CLAWHUB_CMD="clawhub"

  while IFS= read -r skill; do
    [[ -z "$skill" ]] && continue
    if [[ -d "$SKILLS_DIR/$skill" ]]; then
      echo "  ✓ $skill"
    elif [[ -n "$CLAWHUB_CMD" ]]; then
      echo "  Installing skill: $skill"
      clawhub install "$skill" || echo "  ⚠️  Could not install skill: $skill"
    else
      echo "  ⚠️  Skill '$skill' not found (clawhub not available)"
    fi
  done <<< "$SKILLS"
fi

# ── Environment variable check ───────────────────────────────────────────────
ENV_REQUIRED=$(read_field env_required)
ENV_OPTIONAL=$(read_field env_optional)

if [[ -n "$ENV_REQUIRED" ]]; then
  echo ""
  echo "🔑 Required environment variables..."
  MISSING_ENV=()
  while IFS= read -r var; do
    [[ -z "$var" ]] && continue
    if [[ -n "${!var:-}" ]]; then
      echo "  ✓ $var"
    else
      MISSING_ENV+=("$var")
      echo "  ❌ $var (NOT SET)"
    fi
  done <<< "$ENV_REQUIRED"

  if [[ ${#MISSING_ENV[@]} -gt 0 ]]; then
    echo ""
    echo "⚠️  Missing required env vars: ${MISSING_ENV[*]}"
    echo "   Add them to ~/.rcc/.env or export before running"
  fi
fi

if [[ -n "$ENV_OPTIONAL" ]]; then
  echo ""
  echo "🔑 Optional environment variables..."
  while IFS= read -r var; do
    [[ -z "$var" ]] && continue
    if [[ -n "${!var:-}" ]]; then
      echo "  ✓ $var"
    else
      echo "  ○ $var (not set)"
    fi
  done <<< "$ENV_OPTIONAL"
fi

echo ""
echo "✅ SBOM installation complete for agent: $AGENT"
