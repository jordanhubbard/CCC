#!/bin/bash
# register-agent.sh — Register this node with CCC
# Run after setup-node.sh and filling in .env

set -e

ACC_DEST="${HOME}/.acc"
[[ -d "$ACC_DEST" ]] || ACC_DEST="${HOME}/.ccc"
ENV_FILE="${ACC_DEST}/.env"

if [ -f "$ENV_FILE" ]; then
  set -a; source "$ENV_FILE"; set +a
fi

# Support both ACC_URL (new) and CCC_URL (legacy)
ACC_URL="${ACC_URL:-${CCC_URL:-}}"
ACC_ADMIN_TOKEN="${ACC_ADMIN_TOKEN:-${CCC_ADMIN_TOKEN:-}}"

if [ -z "$ACC_URL" ] || [ -z "$AGENT_NAME" ]; then
  echo "ERROR: Set ACC_URL and AGENT_NAME in $ENV_FILE first"
  exit 1
fi

echo "Registering agent '$AGENT_NAME' with $ACC_URL..."

# Prompt for admin token if not set
if [ -z "$ACC_ADMIN_TOKEN" ]; then
  read -rsp "CCC admin token: " ACC_ADMIN_TOKEN; echo
fi

RESPONSE=$(curl -s -X POST "$ACC_URL/api/agents/register" \
  -H "Authorization: Bearer $ACC_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\":\"$AGENT_NAME\",
    \"host\":\"${AGENT_HOST:-$(hostname)}\",
    \"type\":\"${AGENT_TYPE:-full}\",
    \"capabilities\":{
      \"claude_cli\":${AGENT_CLAUDE_CLI:-false},
      \"claude_cli_model\":\"${AGENT_CLAUDE_MODEL:-claude-sonnet-4-6}\",
      \"inference_key\":true,
      \"gpu\":${AGENT_HAS_GPU:-false},
      \"gpu_model\":\"${AGENT_GPU_MODEL:-}\",
      \"gpu_count\":${AGENT_GPU_COUNT:-0},
      \"gpu_vram_gb\":${AGENT_GPU_VRAM_GB:-0},
      \\\"vllm\\\":${VLLM_ENABLED:-false},
      \\\"vllm_model\\\":\\\"${VLLM_MODEL:-}\\\",
      \\\"vllm_served_name\\\":\\\"${VLLM_SERVED_NAME:-}\\\",
      \\\"vllm_port\\\":${VLLM_PORT:-8000}
    },
    \"billing\":{
      \"claude_cli\":\"fixed\",
      \"inference_key\":\"metered\",
      \"gpu\":\"fixed\"
    }
  }")

CCC_AGENT="${CCC_AGENT:-${ACC_DEST}/bin/ccc-agent}"
[ ! -x "$CCC_AGENT" ] && CCC_AGENT="$(command -v ccc-agent 2>/dev/null || echo "")"

TOKEN=$(echo "$RESPONSE" | "$CCC_AGENT" json get .token 2>/dev/null || echo "")

if [ -n "$TOKEN" ]; then
  # Update .env with the issued token (write ACC_AGENT_TOKEN)
  if grep -q "^ACC_AGENT_TOKEN=" "$ENV_FILE"; then
    sed -i "s|^ACC_AGENT_TOKEN=.*|ACC_AGENT_TOKEN=$TOKEN|" "$ENV_FILE"
  elif grep -q "^CCC_AGENT_TOKEN=" "$ENV_FILE"; then
    sed -i "s|^CCC_AGENT_TOKEN=.*|ACC_AGENT_TOKEN=$TOKEN|" "$ENV_FILE"
  else
    echo "ACC_AGENT_TOKEN=$TOKEN" >> "$ENV_FILE"
  fi
  echo "✓ Registered! Agent token saved to $ENV_FILE"
  echo "  Token: $TOKEN"
else
  echo "Registration response: $RESPONSE"
  echo "ERROR: No token in response. Check ACC_URL and admin token."
  exit 1
fi
