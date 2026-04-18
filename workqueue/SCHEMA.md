# Workqueue Schema v1

This document is the canonical reference for `workqueue/queue.json`. All agents should conform to this schema. The `validate-queue.mjs` script enforces it at cron startup.

---

## Top-Level Structure

```json
{
  "version": 1,
  "agent": "rocky",
  "lastSync": "<ISO-8601>",
  "items": [...],
  "completed": [...],
  "syncLog": [...]
}
```

| Field | Type | Required | Owner | Notes |
|-------|------|----------|-------|-------|
| `version` | integer | ✅ | all | Schema version. Currently `1`. Bump on breaking changes. |
| `agent` | string | ✅ | all | Owning agent name (`rocky`, `bullwinkle`, `natasha`). |
| `lastSync` | ISO-8601 string | ✅ | all | Timestamp of last successful outbound sync. |
| `items` | array | ✅ | all | Active queue items (pending/in_progress/blocked). |
| `completed` | array | ✅ | all | Completed/failed/archived items. |
| `syncLog` | array | ✅ | all | Per-cycle sync event log. |

---

## Item Schema

Each item in `items` or `completed`:

```json
{
  "id": "wq-R-010",
  "itemVersion": 1,
  "created": "2026-03-21T04:00:00.000Z",
  "source": "rocky",
  "assignee": "rocky",
  "priority": "normal",
  "status": "pending",
  "title": "Short title",
  "description": "Longer description of the work.",
  "notes": "Free-text agent notes, append-only by convention.",
  "tags": ["infrastructure"],
  "votes": ["rocky", "natasha"],
  "claimedBy": null,
  "claimedAt": null,
  "attempts": 0,
  "maxAttempts": 1,
  "lastAttempt": null,
  "completedAt": null,
  "result": null
}
```

### Required Fields

| Field | Type | Notes |
|-------|------|-------|
| `id` | string | Agent-prefixed (`wq-R-*`, `wq-B-*`, `wq-N-*`) or date-prefixed (`wq-YYYYMMDD-NNN`) for cross-agent items. Must be unique across all items + completed. |
| `itemVersion` | integer ≥ 1 | Increment on every mutation. Used for merge conflict resolution (prefer higher version; on tie, prefer newer timestamp fields). |
| `created` | ISO-8601 string | When the item was first added to the queue. Never mutated after creation. |
| `source` | string | Agent that originally proposed the item (`rocky`, `bullwinkle`, `natasha`, `jkh`). |
| `assignee` | string | Responsible agent: `rocky`, `bullwinkle`, `natasha`, `all`, or `jkh`. |
| `priority` | enum | One of: `urgent`, `high`, `normal`, `low`, `idea`. See Priority section. |
| `status` | enum | One of: `pending`, `in_progress`, `completed`, `failed`, `blocked`, `deferred`. See Status section. |
| `title` | string | Short human-readable title (≤120 chars recommended). |
| `attempts` | integer ≥ 0 | How many times this item has been attempted. |
| `maxAttempts` | integer ≥ 1 | Maximum attempts before marking `failed`. Default: `1`. |

### Optional Fields

| Field | Type | Notes |
|-------|------|-------|
| `description` | string | Detailed description of the work. |
| `notes` | string | Append-only agent log. Separate entries with newlines + timestamps. |
| `tags` | string[] | Free-form categorization tags. |
| `votes` | string[] | Agent/user names who have endorsed this item. Quorum (≥2) promotes `idea` → `pending`. |
| `claimedBy` | string\|null | Agent that has claimed this item. Set before starting work. |
| `claimedAt` | ISO-8601\|null | When the claim was made. Claims >15 min old are stale and may be reset. |
| `lastAttempt` | ISO-8601\|null | When the most recent attempt started. |
| `completedAt` | ISO-8601\|null | When the item reached `completed` or `failed` status. |
| `result` | string\|null | Outcome summary. Required when `status == "completed"` or `"failed"`. |
| `channel` | string | Originating channel (e.g., `mattermost`). |
| `epic` | string | Epic grouping identifier (e.g., `workqueue-reliability`). |
| `dependsOn` | string[] | IDs of items that must be `completed` before this one can start. |
| `preferred_executor` | string | Soft executor hint — one of the values in `ExecutorType` (see below). The dispatcher uses this if the agent supports it; ignored if unsupported. |
| `required_executors` | string[] | Hard executor filter — agent MUST support at least one of these to claim this item. Dispatcher picks the first supported type. Items with `required_executors` that no online agent supports stay `pending`. |
| `acks` | object | **Directive items only** (`source: "jkh"`, `assignee: "all"`). Dict of `{ agentName: ISO-8601 }` — when each agent acknowledged and applied the directive. Item is considered `fully_resolved` when all three agents (rocky, bullwinkle, natasha) have entries. Agents add their ack in the next sync message after processing the directive. |

---

## Executor Types

Valid values for `preferred_executor` and `required_executors`:

| Value | Backend | Auth | Notes |
|-------|---------|------|-------|
| `claude_cli` | `claude --print` subprocess | `~/.claude/credentials` or `ANTHROPIC_API_KEY` | Current default |
| `claude_sdk` | `@anthropic-ai/claude-code` `query()` | Same `~/.claude/credentials` | Structured output, cost metadata, session IDs |
| `codex_cli` | `codex --approval-mode full-auto` | `OPENAI_API_KEY` | OpenAI billing |
| `codex_vllm` | `codex` → local vLLM endpoint | None (local) | Fixed cost; requires GPU node |
| `cursor_cli` | `cursor --headless` | `CURSOR_SESSION_TOKEN` | Experimental (2026-Q1) |
| `opencode` | `opencode run` → ollama / vLLM | `OPENAI_API_KEY` or `none` | Local-first fallback chain |
| `inference_key` | Direct API call (no coding agent) | `NVIDIA_API_KEY` / cloud key | Non-coding tasks |

---

## Priority Values

| Value | Meaning |
|-------|---------|
| `urgent` | Drop everything. DM assignee immediately. |
| `high` | Process before normal items. |
| `normal` | Standard work. |
| `low` | Do when nothing else is pending. |
| `idea` | Proposal awaiting peer votes. Not actionable until promoted. |

Auto-escalation thresholds (via `priority-escalation.mjs`):
- `idea` → `low` after 72h pending
- `low` → `normal` after 48h
- `normal` → `high` after 24h
- `high` → capped (no auto-`urgent`; urgent is human-only)

---

## Status Values

| Value | Meaning |
|-------|---------|
| `pending` | Waiting to be claimed and worked. |
| `in_progress` | Claimed and actively being processed. |
| `completed` | Done. `result` and `completedAt` must be set. |
| `failed` | Failed after `maxAttempts`. `result` should contain error detail. |
| `blocked` | Waiting on external dependency or human action. |
| `deferred` | Agent lacks required tools; waiting for capability. |

---

## SyncLog Entry Schema

```json
{
  "ts": "2026-03-21T07:00:00.000Z",
  "peer": "natasha",
  "channel": "agentbus",
  "direction": "outbound",
  "success": true,
  "itemCount": 28,
  "note": "Optional free-text note."
}
```

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `ts` | ISO-8601 | ✅ | When the sync event occurred. |
| `peer` | string | ✅ | Peer name (`bullwinkle`, `natasha`). |
| `channel` | string | ✅ | Channel used (`agentbus`, `p2p`, `slack`). |
| `direction` | enum | ✅ | `outbound` or `inbound`. |
| `success` | boolean | ✅ | Whether the sync succeeded. |
| `itemCount` | integer | ✅ | Number of items in the sync payload. |
| `note` | string | ❌ | Optional context. |

---

## ID Conventions

- **Agent-specific items:** `wq-{INITIAL}-{NNN}` — e.g., `wq-R-010`, `wq-B-003`, `wq-N-006`
  - `R` = Rocky, `B` = Bullwinkle, `N` = Natasha
  - Counter is per-agent, monotonically increasing
- **Cross-agent / jkh items:** `wq-YYYYMMDD-NNN` — e.g., `wq-20260319-007`

---

## Merge Conflict Resolution

When merging items from a peer sync:
1. Dedup by `id`
2. Keep the item with the higher `itemVersion`
3. On `itemVersion` tie: prefer the item with the newer `claimedAt` or `lastAttempt` timestamp
4. Never downgrade `status` (e.g., don't revert `completed` → `pending`)

---

## Claim Staleness

A claim is stale if `claimedAt` is set, `status` is still `pending` or `in_progress`, and `now - claimedAt > 15 minutes`. Any agent may reset a stale claim (clear `claimedBy` and `claimedAt`, increment `itemVersion`).

---

*Schema maintained by Rocky. Last updated: 2026-03-21.*
