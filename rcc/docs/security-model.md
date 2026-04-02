# RCC Security Model

Claw Command Center uses a layered token hierarchy to keep secrets
centralized, distribute only what each agent needs, and enable rotation
without re-provisioning.

---

## Token Hierarchy

```
┌─────────────────────────────────────────────────────────────────────┐
│                         jkh (operator)                              │
│                                                                     │
│  RCC_ADMIN_TOKEN ──────────────────────────── held in ~/.rcc/.env  │
│  (never distributed, never in logs, never in git)                  │
└────────────────────────────┬────────────────────────────────────────┘
                             │ issues
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     RCC_BOOTSTRAP_TOKEN                             │
│                                                                     │
│  • Short-lived (default TTL: 1 hour)                                │
│  • Single-use (consumed on first GET /api/bootstrap?token=...)      │
│  • Scoped to one agent name                                         │
│  • Created via: POST /api/bootstrap/token  (admin auth required)    │
│  • Delivered out-of-band to the agent operator (paste in terminal,  │
│    QR code, or encrypted channel — never committed to git)          │
└────────────────────────────┬────────────────────────────────────────┘
                             │ exchanged for
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      RCC_AGENT_TOKEN                                │
│                                                                     │
│  • Long-lived per-agent bearer token                                │
│  • Returned in the bootstrap response (once bootstrap token is used)│
│  • Written to ~/.rcc/.env on the agent machine (chmod 600)          │
│  • Used for all subsequent API calls to RCC                         │
│  • Stored in RCC's agents.json (persists across RCC restarts)       │
│  • Authenticates: heartbeat, queue ops, secrets fetch, lessons      │
│  • Rotation: issue a new bootstrap token, re-run bootstrap.sh       │
└────────────────────────────┬────────────────────────────────────────┘
                             │ used to fetch
                             ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    All Other Secrets                                │
│                                                                     │
│  Stored in:  rcc/data/secrets.json  (chmod 600, gitignored)         │
│                                                                     │
│  Named bundles (each returns a set of related env vars):            │
│    slack       SLACK_BOT_TOKEN, SLACK_SIGNING_SECRET, etc.          │
│    (mattermost retired 2026-04-01)                                  │
│    minio       MINIO_ACCESS_KEY, MINIO_SECRET_KEY, MINIO_ENDPOINT   │
│    milvus      MILVUS_ADDRESS                                       │
│    nvidia      NVIDIA_API_KEY, NVIDIA_API_BASE                      │
│    github      GITHUB_TOKEN (deploy key handled separately)         │
│                                                                     │
│  API (auth: agent token):                                           │
│    GET /api/secrets/:key   — fetch by key or named alias            │
│                                                                     │
│  API (auth: admin token only):                                      │
│    GET /api/secrets        — list all secret keys (not values)      │
│    POST /api/secrets/:key  — write/update a secret                  │
│                                                                     │
│  Rotation flow:                                                     │
│    1. Admin POSTs new value to /api/secrets/:key                    │
│    2. agent-pull.sh (cron, ~10 min) calls GET /api/secrets/:alias   │
│       for each bundle and refreshes ~/.rcc/.env automatically       │
│    3. Services are restarted automatically if config changed        │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Token Properties Comparison

| Token              | Holder        | Lifetime     | Single-use | Grants                          |
|--------------------|---------------|--------------|------------|----------------------------------|
| RCC_ADMIN_TOKEN    | jkh / RCC srv | Permanent    | No         | Full API access + write secrets  |
| RCC_BOOTSTRAP_TOKEN| Provisioner   | Short (1h)   | Yes        | One-time agent provisioning      |
| RCC_AGENT_TOKEN    | Each agent    | Long-lived   | No         | Queue/heartbeat/secrets (read)   |

---

## Secrets at Rest

- `~/.rcc/.env` — agent machine, chmod 600, never committed to git
- `rcc/data/secrets.json` — RCC server, chmod 600, gitignored
- `rcc/data/github-key.json` — RCC server, chmod 600, gitignored
- `rcc/api/agents.json` — contains per-agent tokens, excluded from git

Secrets must **never** appear in:
- Git history (`.gitignore` guards this)
- Application logs (token fields are not logged)
- Process arguments (tokens are passed via env, not `--arg=TOKEN`)

---

## Bootstrap Flow

```
  jkh creates bootstrap token via:
    POST /api/bootstrap/token  { agent: "boris", ttlSeconds: 3600 }
    ← { bootstrapToken: "rcc-bootstrap-boris-abc12345", expiresAt: "..." }

  jkh delivers token to boris operator out-of-band.

  On boris machine:
    bash bootstrap.sh --rcc=http://... --token=rcc-bootstrap-boris-abc12345 --agent=boris

  bootstrap.sh:
    1. Installs OpenClaw
    2. Clones workspace
    3. Calls GET /api/bootstrap?token=rcc-bootstrap-boris-abc12345
       ← agentToken, deployKey, rccUrl (bootstrap token is now consumed)
    4. Writes ~/.rcc/.env with RCC_AGENT_TOKEN=<agentToken>
    5. Fetches all named secret bundles using agentToken
    6. Appends service credentials to ~/.rcc/.env
    7. Starts OpenClaw gateway
    8. Posts first heartbeat
```

---

## Secrets Rotation

To rotate a service credential (e.g. Slack bot token):

```bash
# On RCC server (jkh, with admin token):
curl -X POST http://localhost:8789/api/secrets/slack \
  -H "Authorization: Bearer $RCC_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"secrets": {"SLACK_BOT_TOKEN": "xoxb-new-token-here", ...}}'
```

Within ~10 minutes, all agents running `agent-pull.sh` via cron will
automatically pick up the new values from `GET /api/secrets/slack` and
refresh their `~/.rcc/.env`. Services are reloaded if relevant files changed.

To rotate an agent token, issue a new bootstrap token and re-run bootstrap.sh
(or manually update `~/.rcc/.env` and notify RCC via `/api/agents/:name`).

---

## What Agents Must NOT Do

- Store secrets in SOUL.md, TOOLS.md, MEMORY.md, or any committed file
- Pass tokens as command-line arguments (visible in `ps aux`)
- Log token values (even partial)
- Distribute their RCC_AGENT_TOKEN to other agents
- Write to `/api/secrets` (write is admin-only)

See `DIRECTIVES.md` § D-001 for the full directive.

---

*Document version: 1.0 — 2026-03-27*
