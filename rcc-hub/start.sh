#!/bin/bash
# RCC Hub start script — loads .env and launches the API server
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

if [ ! -f .env ]; then
  echo "No .env found. Run: node scripts/setup.mjs"
  exit 1
fi

# Load .env (skip comments and blank lines)
set -a
# shellcheck disable=SC1091
source <(grep -v '^#' .env | grep -v '^$')
set +a

echo "🐿️  Starting RCC Hub on port ${RCC_PORT:-8789}..."
exec node src/api/index.mjs
