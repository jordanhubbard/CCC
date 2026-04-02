# DIRECTIVES.md — Agent Operating Rules

These are standing orders for all agents in the Rocky & Friends crew.
Directives are authoritative: they override convenience, habit, and inference.

---

## D-001 — Secrets Management

**Agents must not store secrets locally beyond what RCC provides.**

The Claw Command Center (RCC) is the sole source of truth for all shared
credentials (API keys, tokens, signing secrets, storage credentials).

### Rules

1. **Do not hardcode secrets** in code, SOUL.md, TOOLS.md, or any committed file.

2. **Do not store secrets in config files** unless they were placed there by RCC
   (bootstrap.sh, migrate.sh, or agent-pull.sh).

3. **If a secret is needed and absent from `~/.rcc/.env`**, fetch it from RCC:

   ```bash
   curl -sf -H "Authorization: Bearer $RCC_AGENT_TOKEN" \
     "$RCC_URL/api/secrets/<key>"
   ```

   Named bundles (return multiple related env vars at once):
   - `GET /api/secrets/slack`       → Slack bot tokens + signing secret
   - `GET /api/secrets/mattermost`  → Mattermost token + URL
   - `GET /api/secrets/minio`       → MinIO access key + secret
   - `GET /api/secrets/milvus`      → Milvus address
   - `GET /api/secrets/nvidia`      → NVIDIA API key + base URL
   - `GET /api/secrets/github`      → GitHub token / deploy key

4. **Secrets are refreshed automatically** on every `agent-pull.sh` run (every
   ~10 minutes). After rotation, the new values propagate to all agents within
   one pull cycle.

5. **RCC_AGENT_TOKEN itself** is the one identity key agents hold locally.
   It is written once at bootstrap and never overwritten by the secrets sync.
   It authenticates the agent to RCC for all subsequent requests.

### Token Hierarchy (summary)

```
RCC_ADMIN_TOKEN      ← jkh only; never leaves the RCC server
RCC_BOOTSTRAP_TOKEN  ← short-lived, single-use; passed at provisioning time
RCC_AGENT_TOKEN      ← long-lived per-agent; returned at bootstrap, kept in ~/.rcc/.env
All other secrets    ← fetched from GET /api/secrets/:key using RCC_AGENT_TOKEN
```

See `rcc/docs/security-model.md` for the full model.

---

## D-002 — No Unapproved External Actions

Agents must not send emails, post to social media, or take any action that
leaves the controlled infrastructure without explicit instruction from jkh.
Internal RCC/ClawBus/Slack/Mattermost comms are fine.

---

## D-003 — Heartbeat Discipline

Agents must post a heartbeat to `POST $RCC_URL/api/heartbeat/$AGENT_NAME` at
least once every 10 minutes. Loss of heartbeat triggers an offline alert.
`agent-pull.sh` (cron) handles this automatically.

---

## D-004 — Work Queue First

Before starting unsolicited work, check the RCC work queue:

```bash
curl -sf -H "Authorization: Bearer $RCC_AGENT_TOKEN" "$RCC_URL/api/queue"
```

Claim items before working. Update status to `in-progress` on claim, then
`completed` (or `failed`) when done. Do not abandon claimed items silently.

---

*Last updated: 2026-03-27 by Rocky (wq-RCC-secrets-design-001)*
