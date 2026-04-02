# RFC: Gateway Provider Routing — Local-First Inference

**Status:** Draft  
**Author:** Natasha (sparky)  
**Date:** 2026-03-29  
**Refs:** `docs/token-bus-architecture.md`, `wq-API-1774746818635`

---

## Problem

All agents currently route inference through a single metered provider
(`nvidia/azure/anthropic/claude-sonnet-4-6`). Sparky has a GB10 with 128GB
unified memory and multiple capable local models via ollama. There is no
mechanism for the gateway to prefer local inference over metered, or to
fall back gracefully when local is unavailable.

Consequences:
- Every token costs money even when a capable local model is available
- Local models (qwen2.5-coder:32b, whisper-large-v3) are used ad-hoc, not
  systematically
- No cost visibility — agents can't see when they're burning metered quota

---

## Proposed Design

### 1. Provider Registry Entry Format

Add a `local` provider kind alongside existing `openai-completions` providers:

```json
"providers": {
  "sparky-ollama": {
    "baseUrl": "http://sparky.tail407856.ts.net:11434/v1/",
    "apiKey": "ollama",
    "api": "openai-completions",
    "kind": "local",
    "host": "sparky",
    "costPerMToken": 0,
    "models": [
      { "id": "qwen2.5-coder:32b", "name": "Qwen2.5 Coder 32B (local)" },
      { "id": "qwen3-coder:latest", "name": "Qwen3 Coder (local)" }
    ]
  },
  "nvidia": {
    "baseUrl": "https://inference-api.nvidia.com/v1/",
    "apiKey": "...",
    "api": "openai-completions",
    "kind": "metered",
    "costPerMToken": 3.0,
    "models": [
      { "id": "azure/anthropic/claude-sonnet-4-6", "name": "Claude Sonnet 4.6" }
    ]
  }
}
```

### 2. Routing Policy

Add a `routing` block to `agents.defaults` (and overridable per-agent):

```json
"routing": {
  "strategy": "prefer_local",
  "localHosts": ["sparky"],
  "fallback": "metered",
  "healthCheckIntervalMs": 30000,
  "localTimeoutMs": 5000
}
```

**Strategies:**

| Strategy | Behaviour |
|---|---|
| `prefer_local` | Try local first; fall back to metered on timeout/error. Default. |
| `local_only` | Only local; error if unavailable. Good for cost-gated tasks. |
| `metered_only` | Always metered. Current behaviour. |
| `round_robin` | Spread load across all healthy providers. |
| `cheapest` | Pick provider with lowest `costPerMToken` that can serve the model family. |

### 3. Model Affinity

The gateway matches by model family, not exact id, to enable local substitution:

```
claude-sonnet  →  if local has qwen2.5-coder:32b AND strategy=prefer_local
                  AND task_tags doesn't include "needs_claude"
                  → try local first
```

Agents can pin a specific provider via `model` prefix:
- `nvidia/azure/anthropic/claude-sonnet-4-6` → always metered (explicit provider)
- `local/qwen2.5-coder:32b` → always local (explicit provider)
- `azure/anthropic/claude-sonnet-4-6` → apply routing policy

### 4. Health Checking

The gateway polls each local provider's `/health` or `/api/tags` every 30s.
Unhealthy locals are removed from the routing pool and metered fallback
activates automatically. When local recovers, it re-enters the pool.

```
sparky-ollama health → GET http://sparky.tail407856.ts.net:11434/api/tags
  success: add to pool
  timeout (5s): remove from pool, log warning
  error: remove from pool, increment failure count
```

### 5. Cost Telemetry

Emit per-request cost events to RCC:

```
POST /api/telemetry/inference
{
  "agent": "natasha",
  "provider": "sparky-ollama",
  "model": "qwen2.5-coder:32b",
  "input_tokens": 1024,
  "output_tokens": 512,
  "cost_usd": 0.0,
  "latency_ms": 4200,
  "routed_via": "local",
  "fallback_used": false
}
```

Rocky can aggregate these into the dashboard cost panel.

### 6. Per-Agent Overrides

Individual agents can override routing in their session config:

```json
"agents": {
  "natasha": {
    "routing": { "strategy": "local_only" },
    "models": { "primary": "local/qwen2.5-coder:32b" }
  },
  "bullwinkle": {
    "routing": { "strategy": "metered_only" }
  }
}
```

### 7. RCC Agent Registry Integration

The RCC `/api/agents` endpoint already tracks agent capabilities. Extend it:

```json
{
  "name": "natasha",
  "localInferenceUrl": "http://sparky.tail407856.ts.net:11434/v1/",
  "localModels": ["qwen2.5-coder:32b", "qwen3-coder"],
  "preferLocal": true
}
```

The gateway can pull this at startup to auto-configure local providers for
known agents — zero manual config for new agents that register local endpoints.

---

## Implementation Phases

**Phase 1 — Config only (no code change needed today):**
- Add `sparky-ollama` provider to `openclaw.json`
- Test manual routing via model prefix `local/qwen2.5-coder:32b`

**Phase 2 — Gateway routing strategy (OpenClaw team):**
- Implement `routing.strategy` in gateway provider selection
- Add health check loop
- Emit cost telemetry events

**Phase 3 — RCC integration:**
- Extend `/api/agents` schema with `localInferenceUrl` + `localModels`
- Gateway auto-discovers local providers from RCC registry on startup

---

## Immediate Action (Phase 1, no OpenClaw changes)

Add to `~/.openclaw/openclaw.json` under `models.providers`:

```json
"sparky-ollama": {
  "baseUrl": "http://sparky.tail407856.ts.net:11434/v1/",
  "apiKey": "ollama",
  "api": "openai-completions",
  "models": [
    { "id": "qwen2.5-coder:32b", "name": "Qwen2.5 Coder 32B (local, sparky)" }
  ]
}
```

Then agents can explicitly route with `local/qwen2.5-coder:32b` for coding
tasks, preserving Claude for reasoning-heavy work.

---

## Open Questions for jkh

1. Should `prefer_local` be the default for all agents, or opt-in per-agent?
2. Should local inference count against session token limits, or be uncapped?
3. Is there a minimum quality bar before local can substitute for Claude?
   (e.g. only for `code`, `embed`, `transcribe` task types)
4. Should Rocky and Bullwinkle be able to route inference *through* sparky
   (as a local inference proxy), or should each agent only use its own host's
   local models?

---

## STT Addendum (2026-03-29)

### Local Whisper STT — Already Wired

OpenClaw has a built-in `whisper-cli` provider in `src/media-understanding/runner.ts`.
Activation requires:
1. `whisper-cli` on PATH → symlinked `~/.local/bin/whisper-cli → ~/Src/whisper.cpp/build/bin/whisper-cli` ✅
2. `WHISPER_CPP_MODEL` env var → set in `~/.config/systemd/user/openclaw-gateway.service` ✅
3. `tools.media.audio.providers: ['whisper-cpp', 'openai']` in `openclaw.json` ✅

### STT Provider Priority (same prefer_local pattern as LLM)

```
audio input → try whisper-cli (local, sparky GB10, large-v3)
              → if hasBinary("whisper-cli") && model exists: use it (6.5× real-time, free)
              → else fall back to openai/deepgram/groq (metered)
```

### Capabilities Enabled

- **Voice notes** (mobile → nodes → sparky STT → text → agent)
- **ClawChat voice messages** — transcribe before indexing to Milvus
- **Meeting transcription** — POST audio to `sparky.local:8792/inference` directly
- **Whisper HTTP API** — `POST http://sparky.local:8792/inference` (multipart, `file=@audio.wav`)
  Returns `{"text": "..."}`. Supports `response_format=json|text|srt|vtt`.

### No restart-in-session risk

Use `gateway config.patch` tool for any future config changes — never `systemctl restart`
from within an active session (kills own connection).
