#!/bin/bash
# onboard-sweden-agent.sh — Non-interactive onboarding for Sweden GPU agents
# 
# Usage: ./onboard-sweden-agent.sh <agent_name> <ssh_port> <tunnel_port> <rcc_token> <bot_token> <app_token>
#
# Example:
#   ./onboard-sweden-agent.sh peabody 22307 18081 rcc-agent-Peabody-d04c1c87 xoxb-... xapp-...
#
# Reads Boris's config as template and customizes for the named agent.
# Writes openclaw.json, workspace files, and supervisor vllm-tunnel.conf.
# Restarts the-companion via supervisorctl.

set -euo pipefail

AGENT_NAME="$1"
SSH_PORT="$2"
TUNNEL_PORT="$3"
RCC_TOKEN="$4"
BOT_TOKEN="$5"
APP_TOKEN="$6"

SSH="ssh -o StrictHostKeyChecking=no -p ${SSH_PORT} horde@horde-dgxc.nvidia.com"

# Capitalize agent name for display
AGENT_DISPLAY="${AGENT_NAME^}"
AGENT_EMOJI="🤖"

# Use Boris's Slack app token as template for the gateway-level app token
# (agents share the same workspace, each has its own bot+app tokens)
BORIS_BOT_TOKEN="${BORIS_BOT_TOKEN:-}"
BORIS_APP_TOKEN="${BORIS_APP_TOKEN:-}"

NVIDIA_API_KEY="${NVIDIA_API_KEY:-}"
GATEWAY_TOKEN="${GATEWAY_TOKEN:-}"

echo "=== Onboarding ${AGENT_DISPLAY} (port ${SSH_PORT}, tunnel ${TUNNEL_PORT}) ==="

# ─── 1. Write openclaw.json ───────────────────────────────────────────────────
echo "→ Writing openclaw.json..."
$SSH "cat > ~/.openclaw/openclaw.json" << JSONEOF
{
  "meta": {
    "lastTouchedVersion": "2026.3.28",
    "lastTouchedAt": "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
  },
  "env": {
    "vars": {
      "CCC_URL": "https://api.yourmom.photos",
      "CCC_AGENT_TOKEN": "${RCC_TOKEN}"
    }
  },
  "ui": {
    "assistant": {
      "name": "${AGENT_DISPLAY}",
      "avatar": "${AGENT_EMOJI}"
    }
  },
  "models": {
    "providers": {
      "nvidia": {
        "baseUrl": "https://inference-api.nvidia.com/v1",
        "apiKey": "${NVIDIA_API_KEY}",
        "api": "openai-completions",
        "models": [
          {
            "id": "azure/anthropic/claude-sonnet-4-6",
            "name": "Claude Sonnet 4.6 (NVIDIA)",
            "api": "openai-completions",
            "contextWindow": 200000,
            "maxTokens": 8192
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "nvidia/azure/anthropic/claude-sonnet-4-6",
        "fallbacks": []
      },
      "models": {
        "nvidia/azure/anthropic/claude-sonnet-4-6": {}
      },
      "workspace": "/home/horde/.openclaw/workspace"
    }
  },
  "tools": {},
  "commands": {
    "native": "auto",
    "nativeSkills": "auto",
    "restart": true,
    "ownerDisplay": "raw"
  },
  "channels": {
    "slack": {
      "mode": "socket",
      "webhookPath": "/slack/events",
      "enabled": true,
      "botToken": "${BOT_TOKEN}",
      "userTokenReadOnly": true,
      "groupPolicy": "allowlist",
      "streaming": "partial",
      "nativeStreaming": true,
      "accounts": {
        "omgjkh": {
          "name": "${AGENT_DISPLAY} (omgjkh)",
          "botToken": "${BOT_TOKEN}",
          "appToken": "${APP_TOKEN}",
          "userTokenReadOnly": true,
          "streaming": "partial",
          "nativeStreaming": true
        }
      }
    },
    "mattermost": {
      "enabled": true,
      "dmPolicy": "pairing",
      "groupPolicy": "allowlist"
    }
  },
  "gateway": {
    "mode": "local",
    "auth": {
      "mode": "token",
      "token": "${GATEWAY_TOKEN}"
    }
  },
  "plugins": {
    "entries": {
      "mattermost": {
        "enabled": true,
        "config": {}
      }
    }
  }
}
JSONEOF

# ─── 2. Create workspace ──────────────────────────────────────────────────────
echo "→ Creating workspace..."
$SSH "mkdir -p ~/.openclaw/workspace/memory"

# IDENTITY.md
$SSH "cat > ~/.openclaw/workspace/IDENTITY.md" << IDEOF
# IDENTITY.md - Who Am I?

- **Name:** ${AGENT_DISPLAY}
- **Agent:** ${AGENT_NAME}
- **CCC:** https://api.yourmom.photos
IDEOF

# SOUL.md
$SSH "cat > ~/.openclaw/workspace/SOUL.md" << SOULEOF
# SOUL.md - Who You Are

Your name is ${AGENT_DISPLAY}. You are a member of the Rocky & Friends agent crew.

You are a GPU compute agent running on NVIDIA HORDE-DGXC hardware in Sweden.
Your specialty: large-scale inference (Nemotron-120B FP8), GPU workloads, vLLM serving.

Be direct, competent, and ready to take on compute-heavy tasks.
SOULEOF

# HEARTBEAT.md
$SSH "cat > ~/.openclaw/workspace/HEARTBEAT.md" << HBEOF
# HEARTBEAT.md

## Each heartbeat:
1. POST heartbeat to CCC:
   \`curl -s -X POST https://api.yourmom.photos/api/heartbeat/${AGENT_NAME} -H "Content-Type: application/json" -H "Authorization: Bearer ${RCC_TOKEN}" -d "{\"status\":\"online\",\"host\":\"${AGENT_NAME}\",\"ts\":\"\$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}"\`
2. POST heartbeat to SquirrelChat:
   \`curl -s -X POST https://chat.yourmom.photos/api/agents/${AGENT_NAME}/heartbeat -H "Content-Type: application/json" -d "{\"status\":\"online\",\"host\":\"${AGENT_NAME}\"}" \`
3. Check CCC health: \`curl -s https://api.yourmom.photos/health\`
4. Check queue: \`curl -s https://api.yourmom.photos/api/queue -H "Authorization: Bearer ${RCC_TOKEN}"\`
5. Claim and work any pending items assigned to ${AGENT_NAME} or all
HBEOF

# TOOLS.md
$SSH "cat > ~/.openclaw/workspace/TOOLS.md" << TOOLSEOF
# TOOLS.md - Local Notes

## Hardware
- **Host:** HORDE-DGXC (Sweden), container ${AGENT_NAME}
- **GPUs:** 4x NVIDIA L40 (~191GB VRAM total)
- **RAM:** 256GB
- **Access:** ssh -o StrictHostKeyChecking=no -p ${SSH_PORT} horde@horde-dgxc.nvidia.com

## vLLM
- Local endpoint: http://localhost:8080/v1
- Tunnel port: ${TUNNEL_PORT} → Rocky (do-host1 localhost:${TUNNEL_PORT})
- Model: NVIDIA-Nemotron-3-Super-120B-A12B-FP8

## CCC
- URL: https://api.yourmom.photos
- Agent token: ${RCC_TOKEN}
TOOLSEOF

# USER.md
$SSH "cat > ~/.openclaw/workspace/USER.md" << USEREOF
# USER.md - About Your Human

- **Name:** Jordan Hubbard
- **What to call them:** jkh
- **Timezone:** America/Los_Angeles
- **Notes:** Deep into AI/ML infrastructure. Direct communicator. Prefers autonomous execution.
USEREOF

# ─── 3. Supervisor vllm-tunnel.conf ──────────────────────────────────────────
echo "→ Writing vllm-tunnel.conf..."
$SSH "sudo tee /etc/supervisor/conf.d/vllm-tunnel.conf > /dev/null" << SVEOF
[program:vllm-tunnel]
command=ssh -N -T -R ${TUNNEL_PORT}:localhost:8080 -i /home/horde/.ssh/${AGENT_NAME}-tunnel -o StrictHostKeyChecking=no -o ServerAliveInterval=30 -o ServerAliveCountMax=3 -o ExitOnForwardFailure=yes -o BatchMode=yes tunnel@146.190.134.110
user=horde
environment=HOME="/home/horde"
directory=/home/horde
stdout_logfile=/tmp/vllm-tunnel.log
stdout_logfile_maxbytes=1MB
redirect_stderr=true
autostart=false
autorestart=true
startretries=999
startsecs=5
priority=60
SVEOF

# ─── 4. Generate SSH tunnel key ───────────────────────────────────────────────
echo "→ Generating SSH tunnel key..."
$SSH "test -f ~/.ssh/${AGENT_NAME}-tunnel || ssh-keygen -t ed25519 -f ~/.ssh/${AGENT_NAME}-tunnel -N '' -C '${AGENT_NAME}-vllm-tunnel' && echo '=== PUBLIC KEY ===' && cat ~/.ssh/${AGENT_NAME}-tunnel.pub"

# ─── 5. Reload supervisor + restart companion ─────────────────────────────────
echo "→ Reloading supervisord and restarting companion..."
$SSH "sudo supervisorctl reread && sudo supervisorctl update && sudo supervisorctl restart companion"

echo ""
echo "✅ ${AGENT_DISPLAY} onboarded!"
echo ""
echo "⚠️  Still needed:"
echo "   1. Add tunnel public key to Rocky's authorized_keys for tunnel user"
echo "   2. sudo supervisorctl start vllm-tunnel  (once tunnel key is authorized)"
echo "   3. Verify: curl https://api.yourmom.photos/api/agents | jq '.[] | select(.name==\"${AGENT_NAME}\")'"
echo ""
