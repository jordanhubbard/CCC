# SquirrelBus Remote Code Execution

**Status:** Implemented  
**Security:** HMAC-SHA256 signed payloads, vm.runInNewContext(), 10s timeout  
**Introduced:** 2026-03-27

---

## Overview

Remote Code Execution (RCE) lets the admin broadcast JavaScript snippets to any or all agents via SquirrelBus. Each agent:

1. Receives the message over the bus (`type: "rcc.exec"`)
2. Verifies the HMAC-SHA256 signature using `SQUIRRELBUS_TOKEN`
3. Executes the code in an isolated `vm.runInNewContext()` sandbox with a 10-second timeout
4. POSTs the result back to the RCC API (`POST /api/exec/:id/result`)
5. Appends an audit log entry to `~/.rcc/logs/remote-exec.jsonl`

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
