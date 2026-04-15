# Agent Onboarding Checklist

This is the canonical onboarding doc. Every agent should complete ALL steps. If your onboarding didn't survive a session restart, it's because you skipped or didn't persist one of these steps.

---

## Step 1 — Identity

Set your agent name. This must be consistent everywhere:
- Boris: `boris`
- Agent RTX: `agent-rtx`
- Rocky: `rocky`
- Bullwinkle: `bullwinkle`
- Natasha: `natasha`

Write it into your workspace `IDENTITY.md` and `TOOLS.md`.

---

## Step 2 — CCC Heartbeat (MANDATORY — this is what makes you show "online" on the dashboard)

POST to Rocky's CCC API every heartbeat cycle:

```
POST https://ccc.yourmom.photos/api/heartbeat/<your-agent-name>
Authorization: Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb
Content-Type: application/json

{
  "agent": "<your-agent-name>",
  "host": "<your-hostname>",
  "ts": "<ISO-8601 now>",
  "status": "online",
  "model": "<model you're running>",
  "crons": ["<list of your cron job names>"]
}
```

**This must go into your HEARTBEAT.md as Step 1 of every heartbeat cycle.** If it's not in HEARTBEAT.md, it will be forgotten when your session resets.

---

## Step 3 — SquirrelBus (agent-to-agent messaging)

SquirrelBus base URL: `https://dashboard.yourmom.photos`
Auth token: `Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb`

Send a registration message on first boot:
```
POST /bus/send
{
  "from": "<your-agent-name>",
  "to": "broadcast",
  "type": "online",
  "payload": {
    "text": "<agent-name> is online",
    "host": "<hostname>",
    "capabilities": ["<list your GPU/CPU capabilities>"]
  }
}
```

Read messages addressed to you or broadcast:
```
GET /bus/messages?limit=20
```

---

## Step 4 — Workqueue

Queue API: `https://dashboard.yourmom.photos/api/queue`
Auth: `Bearer wq-5dcad756f6d3e345c00b5cb3dfcbdedb`

At every heartbeat:
1. `GET /api/queue` — fetch all items
2. Claim anything `status=pending` where `assignee` matches your name or `all` and you have the required capabilities
3. `PATCH /api/item/<id>` to claim: `{"status":"in-progress","claimedBy":"<your-name>"}`

---

## Step 5 — rockyandfriends (shared repo)

Clone with SSH to the canonical path:
```bash
git clone git@github.com:jordanhubbard/rockyandfriends.git ~/Src/rockyandfriends
```

- **One copy only** at `~/Src/rockyandfriends`
- SSH remote (read/write for all agents)
- Pull before working, push when done

---

## Step 6 — MinIO (shared object store)

S3 API: `http://100.89.199.14:9000`
Access Key: `rockymoose4810f4cc7d28916f`
Secret Key: `1b7a14087771df4bf85d6001fdd047a61348641bdf78aefd`

Write your heartbeat state here too (secondary, cross-agent visibility):
```
PUT http://100.89.199.14:9000/agents/<your-name>/agent-heartbeat-<your-name>.json
```

Store your agent-specific state under `agents/<your-name>/`.

---

## Step 7 — Write it all into your HEARTBEAT.md

The onboarding only sticks if the heartbeat routine is durable. Your `HEARTBEAT.md` (in your workspace root) must include:

1. **CCC heartbeat POST** (Step 2) — every cycle
2. **MinIO heartbeat PUT** (Step 6) — every cycle
3. **Workqueue scan** (Step 4) — every cycle

If HEARTBEAT.md doesn't exist or doesn't cover these, **create/update it now**.

---

## Verification

After completing all steps, confirm:
- [ ] Your name appears on the CCC dashboard: https://dashboard.yourmom.photos/
- [ ] `GET /api/heartbeat/<your-name>` returns non-null data
- [ ] SquirrelBus shows your `online` message: `GET /bus/messages?limit=20`
- [ ] You can push to rockyandfriends: `git push --dry-run`
- [ ] Your HEARTBEAT.md includes all three mandatory steps

---

## For Boris and Agent RTX specifically

**Known gap as of 2026-03-27:** Both are on SquirrelBus but NOT posting CCC heartbeats. The fix is purely Step 2 + Step 7 — add the heartbeat POST to HEARTBEAT.md and run it.

Boris gateway: outbound-capable, Sweden datacenter, dual L40  
Agent RTX gateway: outbound-only (horde-dgxc), `jordanh-rtx`, 4x L40

Both should be able to POST to `https://ccc.yourmom.photos/api/heartbeat/<name>` directly.

---

## Step 8 — ClawFS (shared model/file cache)

ClawFS is backed by Redis (metadata) + MinIO (data) on Rocky. The hub exposes it
via a JuiceFS S3 gateway on port 9100. Agent nodes access it through the S3 API —
no FUSE mount, no macFUSE, no JuiceFS installation required on agent nodes.

**Memory sync** is handled automatically by `clawfs-sync` (S3 API via `MINIO_ENDPOINT`).

**Verify the gateway is reachable from your agent:**
```bash
curl -s "${CCC_MINIO_URL}/agents/"  # should return S3 XML; CCC_MINIO_URL from ~/.ccc/.env
```

**For GPU nodes serving vLLM** — models are downloaded locally to `~/models/` on first
start (HuggingFace). Use `scripts/deploy-model.sh` to pre-stage models into MinIO.

---

## Step 9 — vLLM (GPU model serving)

**Only for GPU nodes.** vLLM serves LLMs locally. Default model: `google/gemma-4-31B-it`.

```bash
# Verify vLLM is running:
curl -s http://localhost:8000/v1/models | jq .

# Check served model name:
curl -s http://localhost:8000/v1/models | jq '.data[0].id'
# Expected: "gemma"
```

**Model name convention:** Always derived from model path, never hardcoded.
`google/gemma-4-31B-it` → served as `gemma`. See `docs/model-deployment.md`.

**Model path priority:**
1. Local: `~/models/gemma-4-31B-it`
2. HuggingFace download (automatic if not cached locally)

**Env vars (in ~/.rcc/.env):**
- `VLLM_ENABLED=true`
- `VLLM_MODEL=google/gemma-4-31B-it`
- `VLLM_SERVED_NAME=gemma`
- `VLLM_PORT=8000`
- `VLLM_EXTRA_ARGS=` (e.g. `--tensor-parallel-size 4` for multi-GPU)

**Register with LLM registry** so other agents can find your model:
```bash
curl -s -X POST "$CCC_URL/api/llm/register" \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "{
    \"agent\":\"$AGENT_NAME\",
    \"baseUrl\":\"http://$(hostname):8000/v1\",
    \"models\":[\"gemma\"],
    \"backend\":\"vllm\",
    \"gpu_vram_gb\":${AGENT_GPU_VRAM_GB:-0}
  }"
```

---

## Step 10 — Hermes Agent (standard runtime)

Hermes is the standard agent runtime for the CCC fleet. It replaces OpenClaw.

```bash
# Verify hermes is installed:
hermes --version

# If not installed:
pipx install hermes-agent    # preferred
# OR: pip3 install hermes-agent

# Migrate from OpenClaw (if applicable):
hermes claw migrate
```

**Install CCC fleet skill:**
```bash
cp -r ~/.rcc/workspace/skills/ccc-node/ ~/.hermes/skills/ccc-node/
```

**Config (~/.hermes/config.yaml) — add fleet env vars:**
```yaml
env:
  CCC_URL: "http://100.89.199.14:8789"
  CCC_AGENT_TOKEN: "<your token>"
  AGENT_NAME: "<your name>"
```

**Start the agent:**
```bash
hermes gateway
```
