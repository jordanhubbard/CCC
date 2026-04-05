#!/bin/bash
# register-agent.sh — Register this node with CCC
# Run after setup-node.sh and filling in .env

set -e

RCC_DIR="$HOME/.rcc"
ENV_FILE="$RCC_DIR/.env"

if [ -f "$ENV_FILE" ]; then
  set -a; source "$ENV_FILE"; set +a
fi

if [ -z "$CCC_URL" ] || [ -z "$AGENT_NAME" ]; then
  echo "ERROR: Set CCC_URL and AGENT_NAME in $ENV_FILE first"
  exit 1
fi

echo "Registering agent '$AGENT_NAME' with $CCC_URL..."

# Prompt for admin token if not set
if [ -z "$RCC_ADMIN_TOKEN" ]; then
  read -rsp "CCC admin token: " RCC_ADMIN_TOKEN; echo
fi

RESPONSE=$(curl -s -X POST "$CCC_URL/api/agents/register" \
  -H "Authorization: Bearer $RCC_ADMIN_TOKEN" \
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
      \"gpu_vram_gb\":${AGENT_GPU_VRAM_GB:-0}
    },
    \"billing\":{
      \"claude_cli\":\"fixed\",
      \"inference_key\":\"metered\",
      \"gpu\":\"fixed\"
    }
  }")

TOKEN=$(echo "$RESPONSE" | node -e "process.stdin.setEncoding('utf8');let d='';process.stdin.on('data',c=>d+=c).on('end',()=>{ try { console.log(JSON.parse(d).token||''); } catch(e){} })")

if [ -n "$TOKEN" ]; then
  # Update .env with the issued token
  if grep -q "^CCC_AGENT_TOKEN=" "$ENV_FILE"; then
    sed -i "s|^CCC_AGENT_TOKEN=.*|CCC_AGENT_TOKEN=$TOKEN|" "$ENV_FILE"
  else
    echo "CCC_AGENT_TOKEN=$TOKEN" >> "$ENV_FILE"
  fi
  echo "✓ Registered! Agent token saved to $ENV_FILE"
  echo "  Token: $TOKEN"
else
  echo "Registration response: $RESPONSE"
  echo "ERROR: No token in response. Check CCC_URL and admin token."
  exit 1
fi
