# Agent Capability Registry

The capability registry lets agents advertise what they can do so the CCC work
pump can route tasks to the right agent dynamically, rather than relying on
hardcoded `preferred_executor` fields.

---

## Manifest Schema

Each agent publishes a manifest via `POST /api/capabilities`:

```json
{
  "agent":     "natasha",
  "host":      "natasha.local",
  "executors": ["claude_cli", "gpu"],
  "gpuSpec": {
    "model":   "nvidia-blackwell",
    "vram_gb": 192,
    "count":   1
  },
  "skills":    ["training", "inference", "render", "code", "gpu"],
  "status":    "online",
  "updatedAt": "2026-03-24T12:00:00.000Z"
}
```

| Field       | Type            | Required | Description |
|-------------|-----------------|----------|-------------|
| `agent`     | string          | yes      | Unique agent name |
| `host`      | string          | no       | Hostname or IP |
| `executors` | string[]        | yes      | Executor types supported (see below) |
| `gpuSpec`   | object \| null  | no       | Required when `"gpu"` is in executors |
| `skills`    | string[]        | no       | High-level task categories |
| `status`    | string          | no       | `"online"` \| `"offline"` \| `"busy"` (default: `"online"`) |
| `updatedAt` | ISO string      | —        | Set automatically on publish |

### Executor Types

Valid `executors` values (exact strings — the registry validates these):

| Executor        | Meaning |
|-----------------|---------|
| `claude_cli`    | Agent runs Claude Code in a tmux session |
| `inference_key` | Agent can make NVIDIA/cloud inference API calls |
| `gpu`           | Agent has local GPU hardware for training/render |

> **Note:** `gpuSpec` must be a JSON object (not null/string) when `"gpu"` is in executors.
> Extra fields beyond the schema (e.g. `hardware`, `models`, `routing_notes`) are stored as-is.

### gpuSpec Fields

| Field     | Type   | Description |
|-----------|--------|-------------|
| `model`   | string | GPU model string, e.g. `"nvidia-blackwell"`, `"nvidia-l40"` |
| `vram_gb` | number | Total VRAM in GB across all GPUs |
| `count`   | number | Number of GPUs |

---

## Agent Registration

Agents publish their manifest at startup using the API:

```bash
curl -X POST http://rcc:8789/api/capabilities \
  -H "Authorization: Bearer $RCC_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "agent":     "bullwinkle",
    "host":      "bullwinkle.local",
    "executors": ["claude_cli", "inference_key"],
    "skills":    ["code", "review", "debug", "macos", "ios"],
    "status":    "online"
  }'
```

Re-publishing is an upsert — the manifest is replaced in full and `updatedAt`
is reset to the current time.

Rocky publishes its own manifest automatically when `rcc-api` starts.

---

## API Endpoints

### `POST /api/capabilities` (auth required)

Publish or update an agent's capability manifest.

- **Auth:** Bearer token (same tokens used for other agent endpoints)
- **Body:** Manifest object (see schema above)
- **Returns:** `{ ok: true, manifest: <normalized manifest> }`

### `GET /api/capabilities`

List all registered agent manifests.

- **Auth:** None
- **Returns:** `[manifest, ...]`

### `GET /api/capabilities/:agent`

Get a single agent's manifest.

- **Auth:** None
- **Returns:** manifest object, or `404` if the agent has never published

---

## How Routing Works

When an item enters the queue it carries a `preferred_executor` hint
(`claude_cli`, `inference_key`, or `gpu`).  Agents call
`GET /api/agents/best?executor=<type>` to find who should handle it.

The routing logic:

1. **Check the capability registry** — find agents whose `executors` array
   contains the requested executor type.
2. **Prefer online agents** — cross-reference with heartbeats; agents that
   posted a heartbeat within the last 10 minutes are considered online.
3. **Fall back** — if no online capable agent is found, consider all registered
   agents with the matching executor (they may just be temporarily quiet).
4. **Sort** — GPU tasks are sorted by `gpuSpec.vram_gb` descending;
   `claude_cli` tasks by context-window size; others by first-match.
5. **Legacy fallback** — if an agent has no registry manifest, the old
   `agents.json` capability flags (`gpu`, `claude_cli`, etc.) are used.

The `?task=X` query param is also supported for semantic routing:
- `gpu`, `render`, `training`, `inference` → routes to `gpu` executors
- `code`, `review`, `debug`, `triage`, `claude` → routes to `claude_cli` executors
- Any other string → matched against `skills` array in the manifest

---

## Known Agent Manifests

| Agent       | Executors                       | GPU                  | Skills |
|-------------|----------------------------------|----------------------|--------|
| rocky       | claude_cli, inference_key        | —                    | code, review, debug, triage, ci |
| bullwinkle  | claude_cli, inference_key        | —                    | code, review, debug, macos, ios |
| natasha     | claude_cli, inference_key, gpu   | Blackwell 192GB      | training, inference, render, code, gpu |
| boris       | inference_key, gpu               | 2× L40 96GB total    | render, training, inference, gpu, video |

---

## Registry Persistence

Manifests are written to `rcc/api/data/capabilities-registry.json` on every
`publish()` call and loaded from disk when `rcc-api` starts.  The file is a
plain JSON object keyed by agent name — safe to inspect or edit by hand.

The registry file path can be overridden with the `REGISTRY_PATH` environment
variable.
