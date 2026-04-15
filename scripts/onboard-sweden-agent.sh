#!/bin/bash
# onboard-firewalled-agent.sh — Non-interactive onboarding for firewalled GPU agents
#
# Usage: ./onboard-firewalled-agent.sh \
#          --agent=<name> \
#          --ssh-host=<host> \
#          --ssh-port=<port> \
#          --ssh-user=<user> \
#          --ccc=<CCC_URL> \
#          --token=<CCC_AGENT_TOKEN> \
#          --hub-host=<hub_public_ip_or_hostname> \
#          --tunnel-port=<port_on_hub>
#
# For a firewalled agent node with no inbound network access:
#   1. Bootstraps hermes-agent via bootstrap.sh
#   2. Writes vllm-tunnel supervisor config (for agents serving vLLM)
#   3. Generates SSH tunnel keypair
#   4. Prints the public key so the operator can authorize it on the hub
#
# The hub must have a 'tunnel' OS user with the agent's public key in authorized_keys.
# Port allocation: GET $CCC_URL/api/agents/<name>/tunnel-port on the hub.

set -euo pipefail

AGENT_NAME=""
SSH_HOST=""
SSH_PORT="22"
SSH_USER="$(whoami)"
CCC_URL=""
CCC_TOKEN=""
HUB_HOST=""
TUNNEL_PORT=""

for arg in "$@"; do
  case "$arg" in
    --agent=*)       AGENT_NAME="${arg#--agent=}"       ;;
    --ssh-host=*)    SSH_HOST="${arg#--ssh-host=}"      ;;
    --ssh-port=*)    SSH_PORT="${arg#--ssh-port=}"      ;;
    --ssh-user=*)    SSH_USER="${arg#--ssh-user=}"      ;;
    --ccc=*)         CCC_URL="${arg#--ccc=}"            ;;
    --token=*)       CCC_TOKEN="${arg#--token=}"        ;;
    --hub-host=*)    HUB_HOST="${arg#--hub-host=}"      ;;
    --tunnel-port=*) TUNNEL_PORT="${arg#--tunnel-port=}";;
    *) echo "Unknown argument: $arg" >&2; exit 1 ;;
  esac
done

if [[ -z "$AGENT_NAME" || -z "$SSH_HOST" || -z "$CCC_URL" || -z "$CCC_TOKEN" || -z "$HUB_HOST" || -z "$TUNNEL_PORT" ]]; then
  echo "Usage: $0 --agent=<name> --ssh-host=<host> --ccc=<url> --token=<token> --hub-host=<host> --tunnel-port=<port>"
  exit 1
fi

SSH="ssh -o StrictHostKeyChecking=no -p ${SSH_PORT} ${SSH_USER}@${SSH_HOST}"

echo "=== Onboarding firewalled agent: ${AGENT_NAME} ==="
echo "  SSH:         ${SSH_USER}@${SSH_HOST}:${SSH_PORT}"
echo "  CCC:         ${CCC_URL}"
echo "  Tunnel:      ${TUNNEL_PORT} on ${HUB_HOST}"

# ─── 1. Bootstrap hermes-agent ────────────────────────────────────────────────
echo "→ Bootstrapping hermes-agent..."
$SSH "bash -s" << BSEOF
curl -sSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/deploy/bootstrap.sh | \
  bash -s -- --ccc=${CCC_URL} --agent-token=${CCC_TOKEN} --agent=${AGENT_NAME}
BSEOF

# ─── 2. Supervisor vllm-tunnel.conf (for GPU agents serving vLLM) ─────────────
echo "→ Writing vllm-tunnel.conf..."
$SSH "sudo tee /etc/supervisor/conf.d/vllm-tunnel.conf > /dev/null" << SVEOF
[program:vllm-tunnel]
command=ssh -N -T -R ${TUNNEL_PORT}:localhost:8080 -i /home/${SSH_USER}/.ssh/${AGENT_NAME}-tunnel -o StrictHostKeyChecking=no -o ServerAliveInterval=30 -o ServerAliveCountMax=3 -o ExitOnForwardFailure=yes -o BatchMode=yes tunnel@${HUB_HOST}
user=${SSH_USER}
environment=HOME="/home/${SSH_USER}"
directory=/home/${SSH_USER}
stdout_logfile=/tmp/vllm-tunnel.log
stdout_logfile_maxbytes=1MB
redirect_stderr=true
autostart=false
autorestart=true
startretries=999
startsecs=5
priority=60
SVEOF

# ─── 3. Generate SSH tunnel key ───────────────────────────────────────────────
echo "→ Generating SSH tunnel key..."
$SSH "test -f ~/.ssh/${AGENT_NAME}-tunnel || \
  ssh-keygen -t ed25519 -f ~/.ssh/${AGENT_NAME}-tunnel -N '' -C '${AGENT_NAME}-vllm-tunnel'"
echo ""
echo "=== TUNNEL PUBLIC KEY (add to hub 'tunnel' user authorized_keys) ==="
$SSH "cat ~/.ssh/${AGENT_NAME}-tunnel.pub"
echo "==="

echo ""
echo "Agent ${AGENT_NAME} onboarded!"
echo ""
echo "Still needed:"
echo "  1. Add the tunnel public key above to the hub's 'tunnel' user authorized_keys"
echo "  2. sudo supervisorctl start vllm-tunnel  (once tunnel key is authorized)"
echo "  3. Verify: curl ${CCC_URL}/api/agents | jq '.[] | select(.name==\"${AGENT_NAME}\")'"
echo ""
