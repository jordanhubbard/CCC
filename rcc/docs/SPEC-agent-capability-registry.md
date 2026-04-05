# Agent Capability Registry — Design Spec (wq-N-008)

_Natasha's proposal. Draft 2026-03-24._

## Problem

Agents currently route tasks by gut feel and tribal knowledge. There's no machine-readable record of what each agent can actually do, what hardware they have, or what tools/skills they've loaded. Routing decisions happen informally via chat or hardcoded assumptions.

## Goal

Each agent publishes a structured manifest describing its capabilities. Any agent (or jkh) can query the registry to make informed routing decisions.

## Manifest Schema

```json
{
  "agent": "natasha",
  "host": "sparky",
  "updatedAt": "2026-03-24T09:00:00Z",
  "hardware": {
    "gpu": "NVIDIA GB10",
    "vram_gb": 128,
    "gpu_unified": true,
    "cpu_cores": 20,
    "ram_gb": 128,
    "arch": "aarch64"
  },
  "capabilities": {
    "gpu_inference": true,
    "whisper_transcription": true,
    "image_generation": true,
    "blender_render": true,
    "embeddings": true,
    "omniverse": true,
    "browser_control": false,
    "calendar_access": false,
    "imessage": false,
    "always_on": false
  },
  "skills": ["weather", "coding-agent", "github", "tmux", "slack"],
  "models": {
    "default": "claude-sonnet-4-6",
    "available": ["claude-sonnet-4-6", "llama-3.3-nemotron-super-49b-v1.5"]
  },
  "routing_notes": "GPU-heavy tasks (renders, inference, embeddings). Omniverse fallback when Boris unavailable. NOT for: calendar, iMessage, always-on watchers."
}
```

## Storage

- **MinIO:** `agents/shared/capabilities-{agent}.json` — agent writes its own
- **CCC API (optional):** `GET /api/agents/:id/capabilities` — Rocky serves as aggregator
- **Cache:** Agents refresh their manifest on startup and every 6 hours

## Implementation Plan

### Phase 1: Publish manifests (each agent)
- Natasha writes `agents/shared/capabilities-natasha.json` to MinIO
- Rocky writes `agents/shared/capabilities-rocky.json`
- Bullwinkle writes `agents/shared/capabilities-bullwinkle.json`
- Boris writes `agents/shared/capabilities-boris.json`

### Phase 2: Query support (Rocky)
- Rocky adds `GET /api/capabilities` endpoint returning all agents' manifests
- Rocky adds `GET /api/capabilities/:agent` for individual lookup (e.g. `GET /api/capabilities/natasha`)
- Rocky adds `POST /api/capabilities/:agent` for agents to publish/update their own manifest (Bearer auth)

### Phase 3: Routing integration (all agents)
- Before assigning a task, query the registry: "who can do GPU inference?"
- Update WORKQUEUE_AGENT instructions with routing rules derived from manifests

## Natasha's Manifest (ready to publish)

```json
{
  "agent": "natasha",
  "host": "sparky",
  "updatedAt": "2026-03-24T09:00:00Z",
  "hardware": {
    "gpu": "NVIDIA GB10 (DGX Spark)",
    "vram_gb": 128,
    "gpu_unified": true,
    "cpu_cores": 20,
    "ram_gb": 128,
    "arch": "aarch64",
    "cuda": true,
    "rtx": true
  },
  "capabilities": {
    "gpu_inference": true,
    "whisper_transcription": true,
    "image_generation": true,
    "sdxl": true,
    "blender_render": true,
    "omniverse_kit": true,
    "embeddings": true,
    "browser_control": true,
    "calendar_access": false,
    "imessage": false,
    "google_workspace": false,
    "sonos": false,
    "always_on": false,
    "ssh_access": false
  },
  "skills": ["weather", "coding-agent", "github", "tmux", "slack", "gh-issues"],
  "models": {
    "default": "nvidia/azure/anthropic/claude-sonnet-4-6",
    "available": [
      "nvidia/azure/anthropic/claude-sonnet-4-6",
      "nvcf/nvidia/llama-3.3-nemotron-super-49b-v1.5"
    ]
  },
  "routing_notes": "GPU-heavy tasks: renders (Blender, Omniverse/Kit), image gen (SDXL), Whisper transcription, large model inference, CUDA. Omniverse fallback when Boris unavailable. NOT for: calendar/iMessage/Google Workspace (Bullwinkle only), always-on watchers (Rocky), x86-only tasks."
}
```

## Next Steps

1. ✅ Natasha publishes her manifest to MinIO (Phase 1 — do this now)
2. Send spec to Rocky for Phase 2 API endpoints
3. Coordinate with Bullwinkle and Boris to publish their manifests
4. Update WORKQUEUE routing rules once all manifests are live
