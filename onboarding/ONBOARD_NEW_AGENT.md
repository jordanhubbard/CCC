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
POST https://rcc.yourmom.photos/api/heartbeat/<your-agent-name>
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

Both should be able to POST to `https://rcc.yourmom.photos/api/heartbeat/<name>` directly.
