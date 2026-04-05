# CCC Task Executor Design: Claude Agent SDK vs CLI `--print`

**Author:** Peabody (horde-dgxc)  
**Date:** 2026-04-02  
**Resolves:** wq-API-1774439766007

---

## Summary

CCC currently dispatches coding tasks via `claude --print --permission-mode bypassPermissions` (subprocess). This document evaluates the `@anthropic-ai/claude-code` SDK as an upgrade path, covers Codex and Cursor CLI as first-class executors, and proposes a unified `RccExecutor` interface that routes to the right backend per agent/task config.

**Bottom line:** CLI and SDK are complementary, not competing. Enterprise SSO/billing flows require the CLI; structured output and streaming require the SDK. The right architecture supports both, with routing decided at task-claim time.

---

## Current State

`workqueue/scripts/run-coding-agent.sh` implements a three-tier fallback:

```
Claude CLI (--print) → opencode/ollama → opencode/Boris vLLM
```

This works but has limitations:
- Output is unstructured stdout — cost/token metadata is lost
- No streaming to CCC; caller blocks until subprocess exits
- Session IDs not captured — no resumability
- Throttle detection is grep-based (fragile)
- No Codex or Cursor CLI integration

---

## Claude Agent SDK (`@anthropic-ai/claude-code`)

### What it provides
- `query()` — programmatic equivalent of `claude --print`, returns structured `SDKMessage[]`
- `ClaudeCodeOptions` — model, system prompt, tool config, max turns, cwd, permission mode
- Full cost metadata: `input_tokens`, `output_tokens`, `cache_read_tokens` per message
- Session ID on every response — enables `--resume <id>` for multi-turn tasks
- Streaming via `AbortSignal` / async iteration
- Native TypeScript types — no stdout parsing

### Auth model
The SDK uses the **same auth as the CLI** — it reads `~/.claude/credentials` (set by `claude login`). There is no separate API key path. This is a **feature**, not a bug:

- Enterprise SSO, OAuth, and per-user billing flow through `claude login`
- Org-level billing tracking, Codex/Cursor credentials, and seat-based usage all depend on this
- **Any CCC node that handles org-billed tasks must keep the CLI credential store active**
- SDK calls will fail on nodes without a valid `claude login` session

### When to prefer SDK
| Scenario | Prefer |
|---|---|
| Structured JSON result needed by CCC brain | SDK |
| Cost/token accounting per task | SDK |
| Streaming progress to Mattermost | SDK |
| Session resume for long tasks | SDK |
| Enterprise SSO / org billing node | SDK (uses same credentials) |
| CI/ephemeral container (no login) | CLI with `ANTHROPIC_API_KEY` |
| Offline fallback to local LLM | opencode/vLLM |

---

## Codex CLI

- `codex --approval-mode full-auto -q "<prompt>"` — non-interactive, exits with result on stdout
- Auth: `OPENAI_API_KEY` env var (no login ceremony) or `~/.codex/config.json`
- Supports `--model` flag — can route to any OpenAI-compatible endpoint (including local vLLM)
- No built-in streaming; subprocess stdout
- Best for: tasks that benefit from GPT-4o/o3, OpenAI function-calling, or when Claude is throttled

### Codex → Peabody vLLM routing

```bash
OPENAI_BASE_URL=http://localhost:18081/v1 \
OPENAI_API_KEY=none \
codex --approval-mode full-auto --model nemotron -q "$PROMPT"
```

This routes Codex at the local Nemotron endpoint — free inference, no external quota.

---

## Cursor CLI

- `cursor --headless --task "<prompt>"` (experimental as of 2026-Q1)
- Auth: `CURSOR_SESSION_TOKEN` or `~/.cursor/session`
- Enterprise billing through Cursor Business accounts
- Best for: tasks requiring Cursor's repo-index (semantic search over large codebases)
- **Not yet stable** — recommend `preferred_executor: cursor_cli` only on opt-in basis

---

## Proposed `RccExecutor` Interface

### Schema extension to work items

Add `preferred_executor` field (already present in CCC schema) with enum:

```typescript
type ExecutorType =
  | 'claude_cli'      // claude --print (current default)
  | 'claude_sdk'      // @anthropic-ai/claude-code SDK
  | 'codex_cli'       // codex --approval-mode full-auto
  | 'codex_vllm'      // codex → local vLLM (nemotron)
  | 'cursor_cli'      // cursor --headless (opt-in)
  | 'opencode'        // opencode CLI → ollama/vLLM
  | 'inference_key';  // direct API call, no coding agent
```

### Routing logic (`rcc/executors/dispatch.mjs`)

```javascript
export async function dispatch(item, agentConfig) {
  const exec = item.preferred_executor || agentConfig.defaultExecutor || 'claude_cli';

  switch (exec) {
    case 'claude_sdk':
      return runClaudeSDK(item, agentConfig);

    case 'claude_cli':
      return runClaudeCLI(item, agentConfig);

    case 'codex_cli':
      return runCodex(item, { baseUrl: null });           // OpenAI

    case 'codex_vllm':
      return runCodex(item, { baseUrl: agentConfig.vllmUrl || 'http://localhost:18081/v1' });

    case 'cursor_cli':
      return runCursor(item, agentConfig);

    case 'opencode':
      return runOpencode(item, agentConfig);

    default:
      throw new Error(`Unknown executor: ${exec}`);
  }
}
```

### SDK executor (`rcc/executors/claude-sdk.mjs`)

```javascript
import { query } from '@anthropic-ai/claude-code';

export async function runClaudeSDK(item, agentConfig) {
  const messages = [];
  let totalCost = { input: 0, output: 0, cache_read: 0 };

  for await (const msg of query({
    prompt: item.description,
    options: {
      cwd: agentConfig.repoPath,
      permissionMode: 'bypassPermissions',
      maxTurns: 20,
      model: agentConfig.model,
    },
  })) {
    messages.push(msg);

    // Accumulate token cost
    if (msg.type === 'result') {
      totalCost.input       += msg.usage?.input_tokens       ?? 0;
      totalCost.output      += msg.usage?.output_tokens      ?? 0;
      totalCost.cache_read  += msg.usage?.cache_read_tokens  ?? 0;
    }
  }

  const result = messages.find(m => m.type === 'result');

  return {
    output:     result?.result ?? '',
    sessionId:  result?.session_id ?? null,
    cost:       totalCost,
    executor:   'claude_sdk',
    exitCode:   result?.is_error ? 1 : 0,
  };
}
```

### Codex executor (`rcc/executors/codex.mjs`)

```javascript
import { execFile } from 'child_process';
import { promisify } from 'util';
const exec = promisify(execFile);

export async function runCodex(item, { baseUrl = null } = {}) {
  const env = { ...process.env };
  if (baseUrl) {
    env.OPENAI_BASE_URL = baseUrl;
    env.OPENAI_API_KEY  = env.OPENAI_API_KEY || 'none';
  }

  const { stdout, stderr } = await exec(
    'codex',
    ['--approval-mode', 'full-auto', '-q', item.description],
    { env, cwd: item.repoPath, timeout: 300_000 }
  );

  return {
    output:   stdout,
    executor: baseUrl ? 'codex_vllm' : 'codex_cli',
    exitCode: 0,
  };
}
```

---

## Auth Flow Summary

| Executor | Auth mechanism | Org billing | Works headless |
|---|---|---|---|
| `claude_cli` | `~/.claude/credentials` (via `claude login`) | ✅ Yes | ✅ If pre-logged-in |
| `claude_sdk` | Same `~/.claude/credentials` | ✅ Yes | ✅ If pre-logged-in |
| `codex_cli` | `OPENAI_API_KEY` env | ✅ OpenAI billing | ✅ Yes |
| `codex_vllm` | None (local vLLM, no key) | N/A | ✅ Yes |
| `cursor_cli` | `~/.cursor/session` | ✅ Cursor Business | ⚠️ Experimental |
| `opencode` | `OPENAI_BASE_URL` + `OPENAI_API_KEY` | Depends | ✅ Yes |

**Key constraint:** Claude SDK and CLI both require a pre-authenticated `claude login` session on the executing node. CCC nodes that handle org-billed tasks must maintain this session (consider `claude login --refresh-token` in agent startup scripts).

---

## Recommended Work Items

1. **`rcc/executors/`** — implement `dispatch.mjs`, `claude-sdk.mjs`, `codex.mjs` per designs above
2. **`run-coding-agent.sh`** — refactor to call `dispatch.mjs` via `node -e` or replace with Node wrapper
3. **Brain routing** — when `preferred_executor` is unset, brain should infer from:
   - `has_gpu: true` on agent → prefer `codex_vllm` (free local inference)  
   - `claude login` session present → prefer `claude_sdk`  
   - Neither → `opencode` → ollama fallback
4. **Cost reporting** — SDK `cost` metadata → post to `/api/item/:id/complete` as `cost` field; store in CCC journal for billing dashboards
5. **Session resume** — store `sessionId` from SDK responses; expose `POST /api/item/:id/resume` to continue stalled tasks

---

## Files to Create/Modify

| File | Action |
|---|---|
| `rcc/executors/dispatch.mjs` | **New** — router |
| `rcc/executors/claude-sdk.mjs` | **New** — SDK wrapper |
| `rcc/executors/claude-cli.mjs` | **New** — extract from run-coding-agent.sh |
| `rcc/executors/codex.mjs` | **New** — Codex CLI + vLLM routing |
| `rcc/executors/cursor.mjs` | **New** — Cursor CLI (opt-in) |
| `rcc/executors/opencode.mjs` | **New** — extract from run-coding-agent.sh |
| `workqueue/scripts/run-coding-agent.sh` | **Modify** — thin wrapper calling dispatch |
| `rcc/api/index.mjs` | **Modify** — pass executor result cost to complete handler |
| `package.json` | **Modify** — add `@anthropic-ai/claude-code` dependency |
