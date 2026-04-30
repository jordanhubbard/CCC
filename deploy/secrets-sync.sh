#!/usr/bin/env bash
# secrets-sync.sh — Sync secrets from CCC into ~/.acc/.env
#
# Run this on agent heartbeat to pick up rotated secrets without re-bootstrapping.
# Only updates keys that differ from what's already in .env (minimizes writes).
#
# Usage: bash secrets-sync.sh [--force] [--dry-run]
#   --force    overwrite all keys even if unchanged
#   --dry-run  print what would change, don't write
#
# Requires: ACC_URL and ACC_AGENT_TOKEN in ~/.acc/.env

set -euo pipefail

ACC_DEST="${HOME}/.acc"
[[ -d "$ACC_DEST" ]] || ACC_DEST="${HOME}/.ccc"
ENV_FILE="${ACC_DEST}/.env"
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

ACC_URL="${ACC_URL:-${CCC_URL:-}}"
ACC_AGENT_TOKEN="${ACC_AGENT_TOKEN:-${CCC_AGENT_TOKEN:-}}"

if [[ -z "$ACC_URL" || -z "$ACC_AGENT_TOKEN" ]]; then
  echo "Missing ACC_URL or ACC_AGENT_TOKEN in .env" >&2
  exit 1
fi

# Fetch all secrets from CCC
SECRETS_RESPONSE=$(curl -sf "${ACC_URL}/api/secrets" \
  -H "Authorization: Bearer ${ACC_AGENT_TOKEN}" 2>/dev/null || echo "")

if [[ -z "$SECRETS_RESPONSE" ]]; then
  echo "Failed to fetch secrets from CCC (network error or no secrets configured)" >&2
  exit 0  # non-fatal; agent can still operate with cached .env
fi

CCC_AGENT="${CCC_AGENT:-$ACC_DEST/bin/acc-agent}"
[ ! -x "$CCC_AGENT" ] && CCC_AGENT="$(command -v acc-agent 2>/dev/null || echo "")"

KEYS=$(echo "$SECRETS_RESPONSE" | "$CCC_AGENT" json lines .keys 2>/dev/null || echo "")

if [[ -z "$KEYS" ]]; then
  echo "No secrets in CCC store — nothing to sync"
  exit 0
fi

# Mapping from CCC secret key → .env variable name.
# Keep this as a case statement rather than a Bash associative array: macOS
# still ships Bash 3.x, and the fleet scripts must run there.
mapped_env_key() {
  case "$1" in
    slack/bot_token)          echo "SLACK_BOT_TOKEN" ;;
    slack/app_token)          echo "SLACK_APP_TOKEN" ;;
    slack/signing_secret)     echo "SLACK_SIGNING_SECRET" ;;
    slack/user_token)         echo "SLACK_USER_TOKEN" ;;
    slack/webhook_url)        echo "SLACK_WEBHOOK_URL" ;;
    slack/watch_channel)      echo "SLACK_WATCH_CHANNEL" ;;
    openai/api_key)           echo "OPENAI_API_KEY" ;;
    openai/base_url)          echo "OPENAI_BASE_URL" ;;
    openai/model)             echo "OPENAI_MODEL" ;;
    anthropic/api_key)        echo "ANTHROPIC_API_KEY" ;;
    anthropic/base_url)       echo "ANTHROPIC_BASE_URL" ;;
    anthropic/model)          echo "ANTHROPIC_MODEL" ;;
    hermes/provider)          echo "HERMES_PROVIDER" ;;
    hermes/model)             echo "HERMES_MODEL" ;;
    llm/providers)            echo "LLM_PROVIDERS" ;;
    llm/url)                  echo "LLM_URL" ;;
    llm/key)                  echo "LLM_KEY" ;;
    cli/executor_order)       echo "ACC_CLI_EXECUTOR_ORDER" ;;
    cli/default_executor)     echo "ACC_DEFAULT_CLI_EXECUTOR" ;;
    nvidia/api_key)           echo "NVIDIA_API_KEY" ;;
    nvidia/api_base)          echo "NVIDIA_API_BASE" ;;
    tokenhub/url)             echo "TOKENHUB_URL" ;;
    tokenhub/agent_key)       echo "TOKENHUB_API_KEY" ;;
    agentbus/token)           echo "AGENTBUS_TOKEN" ;;
    squirrelbus/token)        echo "SQUIRRELBUS_TOKEN" ;;
    acc/agent_token)          echo "ACC_AGENT_TOKEN" ;;
    SMB_PASSWORD)             echo "SMB_PASSWORD" ;;
    rocky_smb_user)           echo "SMB_USER" ;;
    rocky_smb_host)           echo "SMB_HOST" ;;
    rocky_smb_share)          echo "SMB_SHARE" ;;
    azure/blob_public_url)    echo "AZURE_BLOB_PUBLIC_URL" ;;
    qdrant/address)           echo "QDRANT_ADDRESS" ;;
    qdrant/embed_model)       echo "EMBED_MODEL" ;;
    qdrant/embed_dim)         echo "EMBED_DIM" ;;
    qdrant/nvidia_embed_url)  echo "NVIDIA_EMBED_URL" ;;
    peers/bullwinkle_url)     echo "BULLWINKLE_URL" ;;
    peers/natasha_url)        echo "NATASHA_URL" ;;
  esac
}

agent_secret_slug() {
  local name="${AGENT_NAME:-${CCC_AGENT_NAME:-$(hostname -s 2>/dev/null || hostname)}}"
  printf '%s' "$name" | tr '[:upper:]' '[:lower:]'
}

workspace_suffix() {
  local workspace="$1"
  workspace="$(printf '%s' "$workspace" | tr '[:lower:]-' '[:upper:]_')"
  printf '_%s' "$workspace"
}

env_keys_for_secret() {
  local secret_key="$1"
  local mapped
  mapped="$(mapped_env_key "$secret_key")"
  if [[ -n "$mapped" ]]; then
    printf '%s\n' "$mapped"
    return 0
  fi

  # Structured Slack workspace keys are stored per bot:
  #   slack/<workspace>/<agent>/<type>
  # Sync only the current agent's keys so every host gets its own app/bot token.
  if [[ "$secret_key" =~ ^slack/([^/]+)/([^/]+)/(app-token|bot-token|signing-secret|user-token|webhook-url|watch-channel|client-id|client-secret|client-signing-secret|client-user-token)$ ]]; then
    local workspace="${BASH_REMATCH[1]}"
    local agent="${BASH_REMATCH[2]}"
    local kind="${BASH_REMATCH[3]}"
    local current_agent
    current_agent="$(agent_secret_slug)"
    agent="$(printf '%s' "$agent" | tr '[:upper:]' '[:lower:]')"
    [[ "$agent" == "$current_agent" ]] || return 0

    local base=""
    case "$kind" in
      app-token)              base="SLACK_APP_TOKEN" ;;
      bot-token)              base="SLACK_BOT_TOKEN" ;;
      signing-secret)         base="SLACK_SIGNING_SECRET" ;;
      user-token)             base="SLACK_USER_TOKEN" ;;
      webhook-url)            base="SLACK_WEBHOOK_URL" ;;
      watch-channel)          base="SLACK_WATCH_CHANNEL" ;;
      client-id)              base="SLACK_CLIENT_ID" ;;
      client-secret)          base="SLACK_CLIENT_SECRET" ;;
      client-signing-secret)  base="SLACK_CLIENT_SIGNING_SECRET" ;;
      client-user-token)      base="SLACK_CLIENT_USER_TOKEN" ;;
    esac
    [[ -n "$base" ]] || return 0

    local env_key="$base"
    if [[ "$workspace" != "omgjkh" && "$workspace" != "default" ]]; then
      env_key="${base}$(workspace_suffix "$workspace")"
    fi
    printf '%s\n' "$env_key"

    # Historical typo compatibility. Older nodes used OFTERRA in env names;
    # write both while all gateway paths are being converged on OFFTERA.
    if [[ "$workspace" == "offtera" && "$env_key" == *"_OFFTERA" ]]; then
      printf '%s\n' "${env_key/_OFFTERA/_OFTERRA}"
    fi
  fi
}

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

  # Look up the .env var name(s)
  env_keys="$(env_keys_for_secret "$secret_key")"
  if [[ -z "$env_keys" ]]; then
    # Unknown secret — skip (don't blindly write unknown keys to .env)
    continue
  fi

  # Fetch the value
  SECRET_VAL=$(curl -sf "${ACC_URL}/api/secrets/${secret_key}" \
    -H "Authorization: Bearer ${ACC_AGENT_TOKEN}" 2>/dev/null | \
    "$CCC_AGENT" json get .value 2>/dev/null || echo "")

  if [[ -z "$SECRET_VAL" ]]; then
    continue
  fi

  while IFS= read -r env_key; do
    [[ -z "$env_key" ]] && continue

    # Check current value
    current_val=$(grep "^${env_key}=" "$ENV_FILE" | head -1 | cut -d'=' -f2- || echo "")

    if [[ "$current_val" == "$SECRET_VAL" ]] && ! $FORCE; then
      unchanged=$((unchanged + 1))
      continue
    fi

    echo "  UPDATE  $env_key"
    set_env_key "$env_key" "$SECRET_VAL"
    updated=$((updated + 1))
  done <<< "$env_keys"
done <<< "$KEYS"

echo "Secrets sync done: $updated updated, $unchanged unchanged."
if $DRY_RUN; then echo "(dry run — no writes made)"; fi
