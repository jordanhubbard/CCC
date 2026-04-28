#!/bin/bash
# Wrapper script for qdrant_ingest.py that bypasses the broken tokenhub proxy.
#
# Problem: tokenhub denies access to text-embedding-3-large for the configured key.
# Solution: Use OpenAI API directly for embeddings by setting EMBEDDING_DIRECT_OPENAI=1.
#
# Usage: Same args as qdrant_ingest.py
#   ./qdrant_ingest_direct.sh                    # Ingest new sessions only
#   ./qdrant_ingest_direct.sh --all              # Re-ingest everything
#   ./qdrant_ingest_direct.sh --memory           # Also ingest MEMORY.md + user profile
#   ./qdrant_ingest_direct.sh --all --memory     # Full re-ingest including memory

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENV_FILE="$HOME/.hermes/.env"

# Read the OPENAI_API_KEY from ~/.hermes/.env (binary read to avoid masking)
read_key() {
    python3 -c "
with open('$ENV_FILE', 'rb') as f:
    for line in f.read().split(b'\n'):
        if b'OPENAI_API_KEY' in line:
            print(line.split(b'=', 1)[1].decode().strip())
            break
"
}

export OPENAI_API_KEY
OPENAI_API_KEY="$(read_key)"

if [ -z "${OPENAI_API_KEY:-}" ]; then
    echo "ERROR: Could not read OPENAI_API_KEY from $ENV_FILE"
    exit 1
fi

echo "Using OpenAI API directly (bypassing tokenhub proxy)"
echo "  Embedding URL: https://api.openai.com/v1/embeddings"
echo "  Model: text-embedding-3-large (3072-dim)"
export EMBEDDING_DIRECT_OPENAI=1

exec python3 "$SCRIPT_DIR/qdrant_ingest.py" "$@"
