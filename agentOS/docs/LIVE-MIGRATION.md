# agentOS Live Slot Migration

**Status:** Implemented (v0.1.0)  
**Author:** Peabody (horde-dgxc)  
**Date:** 2026-03-31  

## Overview

Live migration moves a running WASM agent slot from one agentOS node to another — sparky ↔ do-host1 — without dropping queued tasks. The source slot is frozen, serialized, shipped, and restored on the target node. All observers are notified via ClawBus.

## Why

The agentOS mesh (TransportMesh) can `SPAWN_AGENT` on a remote peer but cannot move a *running* slot. GPU load imbalance across the fleet (sparky GB10 vs do-host1) requires the ability to migrate hot slots without restart.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Source Node (sparky)                        │
│                                                                     │
│  MigrateAgent ─→ VibeSwap.drain(slot)                              │
│               ─→ VibeSwap.getSlot(slot) [with kvStore]             │
│               ─→ snapshot = {wasmHash, kvStore, caps, callCount...} │
│               ─→ POST target/migrate/:id/restore {snapshot}        │
│               ─→ VibeSwap.deleteSlot(slot)  [on target 200 OK]     │
│               ─→ ClawBus.publish(agentos.migrate)               │
└─────────────────────────────────────────────────────────────────────┘
                            │ HTTP
                            ▼
┌─────────────────────────────────────────────────────────────────────┐
│                        Target Node (do-host1)                       │
│                                                                     │
│  MigrateAgent ─→ receive POST /migrate/:id/restore                 │
│               ─→ VibeSwap.load(slot, wasmHash) [fetches AgentFS]   │
│               ─→ restore kvStore, callCount, capabilities           │
│               ─→ return 200 → source tears down                    │
└─────────────────────────────────────────────────────────────────────┘
```

## Services Modified / Added

| Service | Change |
|---------|--------|
| `services/migrate/` | **New** — MigrateAgent REST service (port 8795) |
| `services/vibeswap/index.mjs` | Added: `/slots/:name/drain`, `/slots/:name` DELETE, migration restore path in `/slots/:name/load`, `kvStore` exposed in `getSlot()` |

## API

### MigrateAgent (port 8795)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/migrate/health` | Health check (public) |
| GET | `/migrate` | List recent migrations |
| GET | `/migrate/:id` | Get migration record |
| POST | `/migrate/start` | Start migration from this node |
| POST | `/migrate/:id/restore` | Receive + restore snapshot on this node |

#### Start migration
```json
POST /migrate/start
{ "slotName": "inference-gpu-0", "targetPeer": "http://do-host1:8795" }

→ 202 { "ok": true, "migrationId": "mgr-1744000000-abc123", "status": "snapshotting" }
```

#### Query status
```json
GET /migrate/mgr-1744000000-abc123
→ { "ok": true, "migration": { "id": ..., "status": "completed", "durationMs": 342, ... } }
```

### VibeSwap additions

#### Drain slot (freezes for snapshot)
```json
POST /slots/inference-gpu-0/drain
→ 200 { "ok": true, "slot": { "hash": "...", "kvStore": {...}, "callCount": 42, ... } }
```

#### Delete slot (post-migration teardown)
```json
DELETE /slots/inference-gpu-0
→ 200 { "ok": true, "deleted": "inference-gpu-0" }
```

#### Restore from snapshot
```json
POST /slots/inference-gpu-0/load
{
  "wasmHash": "sha256:abc...",
  "capabilities": ["io.log","kv.get","kv.set"],
  "kvStore": {"42": "hello"},
  "callCount": 42,
  "migratedFrom": "sparky",
  "snapshotAt": "2026-03-31T06:30:00Z"
}
```

## ClawBus Event

Published on successful migration:
```json
{
  "type": "agentos.migrate",
  "source": "sparky",
  "ts": "2026-03-31T06:30:01Z",
  "data": {
    "migrationId": "mgr-...",
    "slotName": "inference-gpu-0",
    "fromPeer": "sparky",
    "toPeer": "http://do-host1:8795",
    "wasmHash": "sha256:abc...",
    "durationMs": 342
  }
}
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `MIGRATE_PORT` | `8795` | Listen port |
| `MIGRATE_TOKEN` | `migrate-dev-token` | Bearer token |
| `VIBESWAP_URL` | `http://localhost:8793` | Local VibeSwap endpoint |
| `AGENTFS_URL` | `http://localhost:8791` | AgentFS (WASM store) |
| `SQUIRRELBUS_URL` | *(unset)* | ClawBus publish URL |
| `AGENT_NODE` | `$AGENT_NAME` | Node identity in events |
| `VIBESWAP_TOKEN` | `$AGENTFS_TOKEN` | VibeSwap auth token |

## Known Limitations (v0.1.0)

1. **WASM memory not snapshotted** — only KV store is migrated. Linear WASM memory state is not serialized. Modules must rebuild computation state from KV on restore.
2. **No rollback** — if restore fails, source slot is not automatically restarted.
3. **No encryption** — snapshot is transmitted as plaintext JSON; use mTLS/Tailscale for transport security.
4. **No mid-restore queuing** — tasks arriving between drain and restore are dropped.

## Roadmap

- [ ] Snapshot linear WASM memory (requires `WebAssembly.Memory` serialization)
- [ ] Automatic rollback on restore failure
- [ ] TransportMesh-native forwarding (bypass raw HTTP)
- [ ] Mid-migration task queuing with replay
