# SquirrelBus Remote Code Execution

**Status:** Implemented  
**Security:** HMAC-SHA256 signed payloads, vm.runInNewContext(), 10s timeout  
**Introduced:** 2026-03-27

---

## Overview

Remote Code Execution (RCE) lets the admin broadcast JavaScript snippets **or shell commands** to any or all agents via SquirrelBus. Each agent:

1. Receives the message over the bus (`type: "rcc.exec"`)
2. Verifies the HMAC-SHA256 signature using `SQUIRRELBUS_TOKEN`
3. Executes the code/command in a sandboxed environment (see Execution Modes)
4. POSTs the result back to the RCC API (`POST /api/exec/:id/result`)
5. Appends an audit log entry to `~/.rcc/logs/remote-exec.jsonl`

## Execution Modes

Set `mode` in the exec payload to control how code runs:

| Mode | Description | Timeout | Auth required |
|------|-------------|---------|---------------|
| `js` (default) | `vm.runInNewContext()` sandbox | 10s | SQUIRRELBUS_TOKEN |
| `shell` | `/bin/sh -c` via allowlist | 30s | SQUIRRELBUS_TOKEN + `ALLOW_SHELL_EXEC=true` |

### Shell Mode

Shell mode lets Rocky or admins run pre-approved commands on remote nodes without needing inbound network access. This is the primary mechanism for administering Sweden containers (peabody, sherman, snidely, dudley) which have no inbound SSH.

**Enable on the target node:**
```bash
export ALLOW_SHELL_EXEC=true
export SHELL_ALLOWLIST="systemctl status,journalctl,df,free,uptime,nvidia-smi,git status,ls,cat,echo,ps aux,curl -s"
```

**Send a shell exec:**
```bash
curl -s -X POST http://146.190.134.110:8789/api/exec \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $RCC_AUTH_TOKEN" \
  -d '{
    "targets": ["peabody"],
    "mode": "shell",
    "code": "nvidia-smi --query-gpu=name,memory.used,memory.free --format=csv,noheader",
    "timeout_ms": 15000
  }'
```

**Security constraints:**
- Commands must start with an allowed prefix from `SHELL_ALLOWLIST`
- No pipes to `sh`, no interactive shells, no token injection
- `eval()` is never used — `/bin/sh -c` via `execFile` (not `exec`)
- All attempts logged regardless of pass/fail

---

## Files

| Path | Description |
|------|-------------|
| `rcc/exec/index.mjs` | HMAC signing/verification library (`signPayload`, `verifyPayload`, `canonicalize`) |
| `rcc/exec/agent-listener.mjs` | Agent-side SquirrelBus subscriber + executor (runs as a daemon) |
| `rcc/api/index.mjs` | API endpoints: `POST /api/exec`, `GET /api/exec/:id`, `POST /api/exec/:id/result` |
| `rcc/docs/remote-exec.md` | This document |
| `rcc/tests/api/exec.test.mjs` | Test coverage |

---

## Security Model

> **NON-NEGOTIABLE rules enforced in code:**
> - Unsigned/tampered payloads are **silently dropped** (no error response to prevent oracle attacks)
> - `eval()` is **never used** — only `vm.runInNewContext()`
> - Every execution attempt (pass or fail) is **logged** to `~/.rcc/logs/remote-exec.jsonl`
> - Hard **10-second timeout** via vm options; timed-out code is killed

### Signature Scheme

1. Build the payload object (without `sig`)
2. `canonicalize()`: deterministic JSON stringify (keys sorted recursively, no whitespace)
3. HMAC-SHA256 over canonical string using `SQUIRRELBUS_TOKEN`
4. Attach as `sig` hex string in the envelope

Verification uses `timingSafeEqual` to prevent timing oracle attacks.

### Sandbox Context

`vm.runInNewContext()` receives a restricted context with no access to:
- `process`, `require`, `import`, `fetch`, `fs`, `net`, `child_process`, etc.

Allowed globals: `Math`, `Date`, `JSON`, `parseInt`, `parseFloat`, `isNaN`, `isFinite`,
`encodeURIComponent`, `decodeURIComponent`, `String`, `Number`, `Boolean`, `Array`, `Object`, `Error`, `console` (captured to output buffer).

---

## API Reference

### POST /api/exec

**Auth:** Admin token required  
**Body:**
```json
{
  "code": "1 + 1",
  "target": "all",
  "replyTo": "optional-context-string"
}
```

**Response:**
```json
{
  "ok": true,
  "execId": "exec-<uuid>",
  "busSent": true
}
```

- Signs the payload with `SQUIRRELBUS_TOKEN`
- Broadcasts as `type: "rcc.exec"` on SquirrelBus
- Appends record to `rcc/api/data/exec-log.jsonl`

### GET /api/exec/:id

**Auth:** Agent token required  
**Response:** Full exec record including accumulated `results[]` from agents.

### POST /api/exec/:id/result

**Auth:** Agent token required  
**Body:**
```json
{
  "agent": "natasha",
  "ok": true,
  "output": "2",
  "result": "2",
  "error": null,
  "durationMs": 3
}
```

Appends the agent result to the exec record.

---

## Running the Agent Listener

```bash
SQUIRRELBUS_TOKEN=your-token \
RCC_AUTH_TOKEN=your-rcc-token \
AGENT_NAME=natasha \
SQUIRRELBUS_URL=http://100.89.199.14:8788 \
RCC_URL=http://100.89.199.14:8789 \
node rcc/exec/agent-listener.mjs
```

Or as a systemd unit / launchd plist alongside the main agent process.

---

## Audit Log Format

Each line in `~/.rcc/logs/remote-exec.jsonl`:

```json
{
  "ts": "2026-03-27T17:00:00.000Z",
  "execId": "exec-<uuid>",
  "agent": "natasha",
  "target": "all",
  "status": "ok",
  "durationMs": 42,
  "output": "hello from natasha",
  "result": "undefined",
  "error": null,
  "codeLen": 32,
  "replyTo": null
}
```

Rejected payloads (bad signature, no secret) are also logged with `"status": "rejected"` and a `"reason"` field.

---

## Example: Broadcast a snippet

```bash
curl -s -X POST http://localhost:8789/api/exec \
  -H "Authorization: Bearer $RCC_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "code": "console.log(\"hello from \" + (typeof AGENT_NAME !== \"undefined\" ? AGENT_NAME : \"sandbox\")); 42",
    "target": "all"
  }'
```

Poll for results:

```bash
EXEC_ID=exec-<uuid>
curl -s http://localhost:8789/api/exec/$EXEC_ID \
  -H "Authorization: Bearer $RCC_AUTH_TOKEN" | jq .results
```

---

## Deployment: Sweden GPU Nodes (peabody / sherman / snidely / dudley)

These nodes have no inbound network access. SquirrelBus exec is the **only** mechanism for remote administration from Rocky or other agents.

### Install agent-listener as a systemd service

```bash
# On the target node (run once during onboarding):
curl -sO https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/rcc/deploy/systemd/agent-listener.service
sudo cp agent-listener.service /etc/systemd/system/
sudo mkdir -p /etc/rcc

# Write credentials (get these from Rocky or jkh):
sudo tee /etc/rcc/env << 'ENVEOF'
SQUIRRELBUS_TOKEN=wq-5dcad756f6d3e345c00b5cb3dfcbdedb
SQUIRRELBUS_URL=http://100.89.199.14:8788
RCC_URL=http://146.190.134.110:8789
RCC_AUTH_TOKEN=<agent-specific-token>
AGENT_NAME=peabody
ALLOW_SHELL_EXEC=true
SHELL_ALLOWLIST=systemctl status,journalctl,df,free,uptime,nvidia-smi,git status,ls,cat,echo,ps aux,curl -s
ENVEOF

sudo systemctl daemon-reload
sudo systemctl enable --now agent-listener
sudo systemctl status agent-listener
```

### Verify from Rocky

```bash
# Check peabody GPU status
curl -s -X POST http://146.190.134.110:8789/api/exec \
  -H "Authorization: Bearer $RCC_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"targets":["peabody"],"mode":"shell","code":"nvidia-smi --query-gpu=name,memory.used --format=csv,noheader"}'
```

### Node names

| Container | AGENT_NAME | GPU | Purpose |
|-----------|-----------|-----|---------|
| peabody | `peabody` | L40 48GB | Primary vLLM serving |
| sherman | `sherman` | L40 48GB | Secondary vLLM serving |
| snidely | `snidely` | L40 48GB | Inference overflow |
| dudley | `dudley` | L40 48GB | Experiments / Boris alt |
