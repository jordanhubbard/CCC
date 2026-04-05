#!/usr/bin/env bash
# secrets-sync.sh — Sync secrets from CCC into ~/.rcc/.env
#
# Run this on agent heartbeat to pick up rotated secrets without re-bootstrapping.
# Only updates keys that differ from what's already in .env (minimizes writes).
#
# Usage: bash secrets-sync.sh [--force] [--dry-run]
#   --force    overwrite all keys even if unchanged
#   --dry-run  print what would change, don't write
#
# Requires: CCC_URL and CCC_AGENT_TOKEN in ~/.rcc/.env

set -euo pipefail

ENV_FILE="${HOME}/.rcc/.env"
DRY_RUN=false
FORCE=false

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=true ;;
    --force)   FORCE=true ;;
  esac
done

if [[ ! -f "$ENV_FILE" ]]; then
  echo "No .env found at $ENV_FILE" >&2
  exit 1
fi

# Load current .env
set -a
# shellcheck source=/dev/null
source "$ENV_FILE"
set +a

CCC_URL="${CCC_URL:-}"
CCC_AGENT_TOKEN="${CCC_AGENT_TOKEN:-}"

if [[ -z "$CCC_URL" || -z "$CCC_AGENT_TOKEN" ]]; then
  echo "Missing CCC_URL or CCC_AGENT_TOKEN in .env" >&2
  exit 1
fi

# Fetch all secrets from CCC
SECRETS_RESPONSE=$(curl -sf "${CCC_URL}/api/secrets" \
  -H "Authorization: Bearer ${CCC_AGENT_TOKEN}" 2>/dev/null || echo "")

if [[ -z "$SECRETS_RESPONSE" ]]; then
  echo "Failed to fetch secrets from CCC (network error or no secrets configured)" >&2
  exit 0  # non-fatal; agent can still operate with cached .env
fi

KEYS=$(echo "$SECRETS_RESPONSE" | node -e "
  let d='';
  process.stdin.on('data', c => d+=c);
  process.stdin.on('end', () => {
    try { console.log(JSON.parse(d).keys.join('\n')); } catch { process.exit(1); }
  });
" 2>/dev/null || echo "")

if [[ -z "$KEYS" ]]; then
  echo "No secrets in CCC store — nothing to sync"
  exit 0
fi

# Mapping from CCC secret key → .env variable name
# Update this map when new secrets are added
declare -A KEY_MAP=(
  ["slack/bot_token_omgjkh"]="OMGJKH_BOT"
  ["slack/bot_token_offtera"]="OFFTERA_BOT"
  ["slack/app_token"]="SLACK_APP_TOKEN"
  ["slack/signing_secret"]="SLACK_SIGNING_SECRET"
  ["slack/omgjkh_user_token"]="OMGJKH_USER_TOKEN"
  ["slack/omgjkh_webhook"]="OMGJKH_WEBHOOK"
  ["slack/watch_channel"]="WATCH_CHANNEL"
  ["mattermost/token"]="MATTERMOST_TOKEN"
  ["mattermost/url"]="MATTERMOST_URL"
  ["nvidia/api_key"]="NVIDIA_API_KEY"
  ["nvidia/api_base"]="NVIDIA_API_BASE"
  ["tokenhub/url"]="TOKENHUB_URL"
  ["tokenhub/agent_key"]="TOKENHUB_AGENT_KEY"
  ["minio/endpoint"]="MINIO_ENDPOINT"
  ["minio/access_key"]="MINIO_ACCESS_KEY"
  ["minio/secret_key"]="MINIO_SECRET_KEY"
  ["minio/bucket"]="MINIO_BUCKET"
  ["agentfs/endpoint"]="AGENTFS_ENDPOINT"
  ["agentfs/access_key"]="AGENTFS_ACCESS_KEY"
  ["agentfs/secret_key"]="AGENTFS_SECRET_KEY"
  ["agentfs/bucket"]="AGENTFS_BUCKET"
  ["azure/blob_public_url"]="AZURE_BLOB_PUBLIC_URL"
  ["milvus/address"]="MILVUS_ADDRESS"
  ["milvus/embed_model"]="EMBED_MODEL"
  ["milvus/embed_dim"]="EMBED_DIM"
  ["milvus/nvidia_embed_url"]="NVIDIA_EMBED_URL"
  ["peers/bullwinkle_url"]="BULLWINKLE_URL"
  ["peers/natasha_url"]="NATASHA_URL"
)

updated=0
unchanged=0

set_env_key() {
  local key="$1" value="$2"
  if $DRY_RUN; then
    echo "  DRY  $key = ${value:0:16}..."
    return
  fi
  # macOS/Linux compat
  if [[ "$(uname)" == "Darwin" ]]; then
    if grep -q "^${key}=" "$ENV_FILE"; then
      sed -i '' "s|^${key}=.*|${key}=${value}|" "$ENV_FILE"
    else
      echo "${key}=${value}" >> "$ENV_FILE"
    fi
  else
    if grep -q "^${key}=" "$ENV_FILE"; then
      sed -i "s|^${key}=.*|${key}=${value}|" "$ENV_FILE"
    else
      echo "${key}=${value}" >> "$ENV_FILE"
    fi
  fi
}

while IFS= read -r secret_key; do
  [[ -z "$secret_key" ]] && continue

  # Look up the .env var name
  env_key="${KEY_MAP[$secret_key]:-}"
  if [[ -z "$env_key" ]]; then
    # Unknown secret — skip (don't blindly write unknown keys to .env)
    continue
  fi

  # Fetch the value
  SECRET_VAL=$(curl -sf "${CCC_URL}/api/secrets/${secret_key}" \
    -H "Authorization: Bearer ${CCC_AGENT_TOKEN}" 2>/dev/null | \
    node -e "let d='';process.stdin.on('data',c=>d+=c);process.stdin.on('end',()=>{try{process.stdout.write(JSON.parse(d).value||'')}catch{}})" 2>/dev/null || echo "")

  if [[ -z "$SECRET_VAL" ]]; then
    continue
  fi

  # Check current value
  current_val=$(grep "^${env_key}=" "$ENV_FILE" | head -1 | cut -d'=' -f2- || echo "")

  if [[ "$current_val" == "$SECRET_VAL" ]] && ! $FORCE; then
    unchanged=$((unchanged + 1))
    continue
  fi

  echo "  UPDATE  $env_key"
  set_env_key "$env_key" "$SECRET_VAL"
  updated=$((updated + 1))
done <<< "$KEYS"

echo "Secrets sync done: $updated updated, $unchanged unchanged."
if $DRY_RUN; then echo "(dry run — no writes made)"; fi
