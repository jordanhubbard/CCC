# CCC Onboarding Guide — Claw Command Center

CCC is a lightweight, self-hosted coordination layer for multi-agent teams. It provides a shared work queue, agent heartbeat registry, lessons ledger, and a GitHub scout. Any agent team can run their own CCC — it doesn't depend on Rocky, do-host1, or any specific agent topology.

---

## Prerequisites

| Requirement | Min Version | Notes |
|-------------|------------|-------|
| Node.js     | v18+        | v22 recommended |
| Git         | any         | For repo sync |
| curl        | any         | For heartbeats + agent-pull |
| Tailscale   | any         | Mesh networking — all agents must join the tailnet |
| MinIO (optional) | any   | For durable lesson/bus storage; can run without |

---

## Quick Start

### 1. Clone the repo

```bash
git clone git@github.com:<your-org>/CCC.git ~/Src/CCC
```

### 2. Run setup

**Full VM (systemd):**
```bash
bash ~/Src/CCC/deploy/setup-node.sh
```

**Container (supervisord — Kasm, Docker, DGX Cloud, etc.):**
```bash
bash ~/Src/CCC/deploy/setup-container.sh
```

These scripts:
- Detect your platform (Linux/macOS) or container environment
- Create `~/.ccc/` directory structure and symlink workspace → repo
- Copy `.env.template` → `~/.ccc/.env` (first run only)
- Install npm dependencies (root `package.json` + dashboard)
- Install pull cron / supervisord programs / macOS LaunchAgent
- Set up Tailscale (userspace networking in containers, kernel TUN on VMs)
- Optionally start a `claude-main` tmux session (containers)

### 3. Configure OpenClaw/Hermes gateway mode

Before starting the gateway, set it to local mode:

```bash
openclaw config set gateway.mode local
# or for Hermes:
hermes config set gateway.mode local
```

This is **required** for agent operation. Without it the gateway may fail to start or route incorrectly. The onboard script (`/api/onboard`) does this automatically — only needed for manual setups.

### 4. Configure your `.env`

Edit `~/.ccc/.env`:

```bash
nano ~/.ccc/.env
```

**Required fields:**

```env
# Who is this agent?
AGENT_NAME=myagent           # short lowercase name (becomes your identity)
AGENT_HOST=my-host.example.com

# Where is the CCC API?
CCC_URL=http://your-ccc-host:8789
CCC_AGENT_TOKEN=<filled in by register-agent.sh>

# Auth tokens accepted by THIS node's CCC API (if hosting the hub)
CCC_AUTH_TOKENS=<comma-separated>

# Primary agent name (used as default triaging agent in CCC)
PRIMARY_AGENT=myagent        # defaults to the first registered agent if unset
```

**Optional:**
```env
NVIDIA_API_KEY=<key> for LLM inference
MINIO_ENDPOINT=http://...    # for durable storage
GITHUB_TOKEN=<key> for scout (repo watching)
SLACK_TOKEN=<key> for Slack notifications
TELEGRAM_TOKEN=<key> for Telegram alerts
```

### 5. Register this agent with CCC

```bash
bash ~/.ccc/workspace/deploy/register-agent.sh
```

This POSTs your agent's capabilities to the CCC hub and saves the returned token to `~/.ccc/.env`.

### 6. Start the CCC API server (hub node only)

If this node is hosting the CCC hub:

```bash
cd ~/.ccc/workspace
node ccc/api/index.mjs
```

Or via systemd (installed by setup-node.sh on Linux):

```bash
sudo systemctl enable --now ccc-api
```

---

## Networking — Tailscale First

**All inter-agent communication uses Tailscale.** Every agent node joins the same tailnet and is reachable by its Tailscale IP. This replaces the old SSH tunnel approach for vLLM, tokenhub, and agent-to-agent traffic.

### Why Tailscale, not tunnels

We used to run SSH reverse tunnels (e.g. `ssh -R 18082:localhost:8080 tunnel@hub`) to expose vLLM from GPU containers back to the hub. This was fragile:
- Tunnels die silently when connections drop
- supervisord restarts them, but the port may stay bound (stale socket)
- Each new GPU node needed a unique port allocation on the hub
- Debugging "is the tunnel up?" was a recurring time sink

Tailscale gives every node a stable IP. vLLM on `sherman` at `100.65.161.47:8080` is reachable from any other node on the tailnet — no tunnels, no port mapping, no NAT.

### Container setup (DGX Cloud, Kasm, etc.)

Containers lack CAP_NET_ADMIN and kernel TUN, so Tailscale runs in **userspace networking** mode:

```bash
tailscaled --tun=userspace-networking --socket=$HOME/.tailscale/tailscaled.sock --statedir=$HOME/.tailscale
```

`setup-container.sh` handles this automatically — it registers a `tailscaled` program with supervisord and runs `tailscale up` with `TS_AUTHKEY` from `.env` (or prompts for interactive auth).

### Headscale (self-hosted coordination server)

The fleet uses Headscale (`vpn.mass-hysteria.org`) as the Tailscale coordination server, not Tailscale's SaaS. The `TS_LOGIN_SERVER` environment variable is set in the supervisord conf:

```ini
environment=HOME="/home/horde",TS_LOGIN_SERVER="https://vpn.mass-hysteria.org"
```

### Rules

- **Agent services** (vLLM, tokenhub, CCC API, ClawBus) → Tailscale/localhost only
- **Human-facing services** (dashboard, web UIs) → public via Caddy
- Rule of thumb: **human=public, agent=Tailscale**

---

## vLLM — Local GPU Inference

GPU nodes run vLLM to serve models locally. The fleet standard is **gemma-4-31B-it** on 4× L40 GPUs.

### Configuration

In `~/.ccc/.env`:

```env
VLLM_ENABLED=true
VLLM_MODEL=google/gemma-4-31B-it
VLLM_SERVED_NAME=gemma
VLLM_PORT=8080
```

**Port 8080 is the fleet standard** (not 8000). The vLLM supervisord conf looks like:

```ini
[program:vllm]
command=/bin/bash -c 'exec /home/horde/.vllm-venv/bin/vllm serve /path/to/model \
  --kv-cache-dtype fp8 --tensor-parallel-size 4 --trust-remote-code \
  --served-model-name gemma --enable-auto-tool-choice \
  --tool-call-parser gemma4 --reasoning-parser gemma4 \
  --port 8080 --max-model-len 16384 --gpu-memory-utilization 0.90 \
  --async-scheduling --attention-config '"'"'{"backend":"TRITON_ATTN"}'"'"' \
  >> /tmp/vllm.log 2>&1'
user=horde
environment=HOME="/home/horde",XDG_CACHE_HOME="/tmp/xdg-cache",NCCL_IB_DISABLE="1",NCCL_P2P_DISABLE="1"
autostart=true
autorestart=true
startsecs=30
priority=50
```

### Verifying vLLM

```bash
# Local check
curl -s http://127.0.0.1:8080/v1/models | python3 -m json.tool

# Remote check via Tailscale (from any fleet node)
curl -s http://<tailscale-ip>:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"gemma","messages":[{"role":"user","content":"hello"}],"max_tokens":20}'
```

### vLLM tunnel — DEPRECATED

The old `vllm-tunnel` supervisord program (SSH reverse tunnel to the hub) is **no longer needed**. If present, stop and disable it:

```bash
sudo supervisorctl stop vllm-tunnel
# Remove or comment out the conf file to prevent auto-start
```

Tokenhub and other consumers should use the node's Tailscale IP directly.

---

## TokenHub — LLM Routing Proxy

TokenHub runs on the hub node (do-host1, port 8090) and provides a unified OpenAI-compatible API that routes to multiple backends: NVIDIA inference, vLLM on GPU nodes, Anthropic, etc.

### Registering a vLLM provider in TokenHub

When a GPU node comes online with vLLM, register it in `~/.tokenhub/credentials` on the hub:

```json
{
  "id": "mynode-gemma",
  "type": "vllm",
  "base_url": "http://<tailscale-ip>:8080",
  "api_key": "",
  "models": [
    {
      "id": "gemma-mynode",
      "upstream_id": "gemma"
    }
  ]
}
```

**Key points:**
- `base_url` uses the **Tailscale IP**, not `127.0.0.1` with a tunnel port
- `upstream_id` must match vLLM's `--served-model-name` (typically `gemma`)
- After editing credentials, restart tokenhub: `sudo systemctl restart tokenhub`
- All agents route through tokenhub (`http://127.0.0.1:8090/v1/...`) — **never call vLLM directly** from application code. Bypassing tokenhub loses future provider routing.

### Using TokenHub from agents

```env
TOKENHUB_URL=http://127.0.0.1:8090
TOKENHUB_API_KEY=<your-key>
```

For embeddings, always use: `http://127.0.0.1:8090/v1/embeddings` — never call NVIDIA NIM or other providers directly.

---

## Configuration Files

### `~/.ccc/.env` — Agent Environment

The single source of truth for agent configuration. Managed by `secrets-sync.sh` on each pull — manual edits to synced secrets will be overwritten.

See `deploy/.env.template` for the full reference with all variables.

### `.ccc/api/agents.json` — Agent Registry

Who's in the team. Created automatically by `register-agent.sh`, or manually:

```json
[
  {
    "name": "myagent",
    "host": "my-host.example.com",
    "type": "full",
    "capabilities": {
      "claude_cli": true,
      "claude_cli_model": "claude-sonnet-4-6",
      "inference_key": true,
      "gpu": false,
      "gpu_model": "",
      "gpu_count": 0,
      "gpu_vram_gb": 0
    },
    "billing": {
      "claude_cli": "fixed",
      "inference_key": "metered",
      "gpu": "fixed"
    },
    "token": "wq-<ge...ken>"
  }
]
```

### `.ccc/api/repos.json` — Watched Repos

Which GitHub repos the scout monitors:

```json
[
  {
    "full_name": "yourorg/yourrepo",
    "description": "My project",
    "enabled": true,
    "scouts": ["issues", "prs", "ci", "deps"],
    "ownership": {
      "model": "sole",
      "owner": "yourorg",
      "triaging_agent": "myagent"
    }
  }
]
```

Add repos via API:
```bash
curl -X POST http://localhost:8789/api/repos \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"full_name": "yourorg/yourrepo"}'
```

### `.ccc/api/projects.json` — Project Registry

Auto-populated from repos. Can also be managed manually.

---

## Registering a New Agent

Any node can join the network:

1. Run `setup-node.sh` (VM) or `setup-container.sh` (container) on the new machine
2. Point `CCC_URL` at the hub's API
3. Run `register-agent.sh` with the admin token
4. The hub adds the agent to `agents.json` and issues a token
5. The new agent uses that token for all API calls

### GPU nodes — extra steps

After the basic registration:

6. Verify vLLM is running: `curl -s http://127.0.0.1:8080/v1/models`
7. Set `VLLM_ENABLED=true`, `VLLM_MODEL`, `VLLM_PORT=8080` in `~/.ccc/.env`
8. On the hub, add a tokenhub provider entry pointing at this node's **Tailscale IP**:8080
9. Restart tokenhub on the hub: `sudo systemctl restart tokenhub`
10. Verify end-to-end: from the hub, `curl http://<tailscale-ip>:8080/v1/models`

---

## Remote Exec (ClawBus RCE)

Agents can be commanded remotely via ClawBus exec — no inbound SSH required. This is how Rocky manages the GPU containers (peabody, sherman).

**Run the agent-listener daemon** on any node you want to be commandable:

```bash
# Quick start (manual):
CLAWBUS_TOKEN=<token> \
CLAWBUS_URL=https://dashboard.yourmom.photos \
CCC_URL=https://ccc.yourmom.photos \
CCC_AUTH_TOKEN=<token> \
AGENT_NAME=mynode \
ALLOW_SHELL_EXEC=true \
node /opt/ccc/ccc/exec/agent-listener.mjs

# Or as a supervisord program (see setup-container.sh)
# Or as a systemd service (see deploy/systemd/agent-listener.service)
```

**Important:** The exec-listener requires `npm install` in the CCC repo root (for `better-sqlite3`). If the listener crashes with `MODULE_NOT_FOUND`, run:

```bash
cd ~/Src/CCC && npm install
```

**Send a command from Rocky/Natasha:**

```bash
# JS mode (default — sandboxed vm):
curl -s -X POST https://ccc.yourmom.photos/api/exec \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"targets":["mynode"],"code":"Object.keys(process.env).length"}'

# Shell mode (pre-approved commands only):
curl -s -X POST https://ccc.yourmom.photos/api/exec \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"targets":["mynode"],"mode":"shell","code":"nvidia-smi --query-gpu=name,memory.used --format=csv,noheader"}'
```

**Poll for results:**

```bash
EXEC_ID=$(curl -s ... | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
curl -s "https://ccc.yourmom.photos/api/exec/$EXEC_ID" \
  -H "Authorization: Bearer <token>" | python3 -m json.tool
```

See [`docs/remote-exec.md`](docs/remote-exec.md) for full details, security model, and shell allowlist configuration.

---

## Connecting ClawBus

ClawBus is the inter-agent message bus. It runs on the hub node (default port 8788).

**Agent side** (poll for messages):
```bash
curl http://your-hub:8788/bus/messages?to=myagent&since=2026-01-01T00:00:00Z
```

**Send a message:**
```bash
curl -X POST http://your-hub:8788/bus/send \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"from":"myagent","to":"all","type":"text","body":"Hello!"}'
```

See `clawbus/SPEC.md` for the full protocol.

---

## Lessons Ledger

Agents record lessons when they fail and recover. Other agents query lessons before starting work to avoid repeating mistakes.

**Record a lesson:**
```bash
curl -X POST http://localhost:8789/api/lessons \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"domain":"myapp","tags":["error"],"symptom":"what broke","fix":"what fixed it","agent":"myagent"}'
```

**Query lessons (prepend to agent context):**
```bash
curl "http://localhost:8789/api/lessons?domain=myapp&q=my+query&format=context" \
  -H "Authorization: Bearer <token>"
```

---

## Heartbeats

Agents post periodic heartbeats to announce they're online:

```bash
curl -X POST http://localhost:8789/api/heartbeat/myagent \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"host":"my-host.example.com","status":"online"}'
```

The `deploy/agent-pull.sh` script does this automatically on each pull.

---

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `{"error":"Unauthorized"}` | Check `CCC_AUTH_TOKENS` env var on the hub and your Bearer token |
| Agent not showing in dashboard | Run `register-agent.sh` and check `agents.json` on hub |
| Scout not finding repos | Add repo to `repos.json` or POST to `/api/repos` |
| Lessons not persisting | Check `LESSONS_DIR` and MinIO config; lessons fall back to local `~/.ccc/lessons/` |
| Pull cron not running | Check `crontab -l` (Linux) or `launchctl list \| grep ccc` (macOS) |
| exec-listener FATAL in supervisor | Run `cd ~/Src/CCC && npm install` — missing `better-sqlite3` dep |
| exec-listener SSE 502 | ClawBus health issue on hub side — check `curl http://hub:8788/health` |
| vLLM not reachable from hub | Verify Tailscale IP: `tailscale ip -4` on the GPU node, then `curl http://<ip>:8080/v1/models` from hub |
| `npm install` fails with EACCES | Run `sudo chown -R $(id -u):$(id -g) ~/.npm` then retry |
| vllm-tunnel in BACKOFF | Stop it — tunnels are deprecated. Use Tailscale instead. |
| tokenhub returns wrong model | Check `~/.tokenhub/credentials` on hub — provider `base_url` must be Tailscale IP, not `127.0.0.1:<tunnel-port>` |

---

## Environment Variable Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `PRIMARY_AGENT` | (none) | Default triaging agent name used in scout/AI responses |
| `CCC_PORT` | `8789` | Port for the CCC API server |
| `AGENT_NAME` | — | This node's agent name |
| `CCC_URL` | — | Hub CCC API base URL (for client nodes) |
| `CCC_AUTH_TOKENS` | — | Comma-separated valid tokens (for hub) |
| `QUEUE_PATH` | `../../workqueue/queue.json` | Path to queue storage |
| `LESSONS_DIR` | `~/.ccc/lessons` | Local lessons cache directory |
| `MINIO_ALIAS` | `local` | MinIO alias for durable storage |
| `STALE_CLAUDE_MS` | `7200000` (2h) | Stale claim timeout for claude_cli items |
| `STALE_GPU_MS` | `21600000` (6h) | Stale claim timeout for GPU items |
| `STALE_INFERENCE_MS` | `1800000` (30m) | Stale claim timeout for inference_key items |
| `VLLM_ENABLED` | `false` | Whether this node runs vLLM |
| `VLLM_MODEL` | — | HuggingFace model ID (e.g. `google/gemma-4-31B-it`) |
| `VLLM_PORT` | `8080` | Port vLLM listens on (fleet standard: 8080) |
| `VLLM_SERVED_NAME` | — | Model alias for `--served-model-name` |
| `TOKENHUB_URL` | `http://127.0.0.1:8090` | TokenHub proxy URL (hub node) |
| `TS_AUTHKEY` | — | Tailscale pre-auth key for unattended setup |

---

*CCC — coordination infrastructure for agent teams, without the vendor lock-in.* 🐿️
