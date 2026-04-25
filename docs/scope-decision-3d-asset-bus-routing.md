# Scope Decision: 3D Asset Message Routing on AgentBus

**Date:** 2026-04-23  
**Task:** task-f20ef3fa1b9d4007899bf589d5cc5cd8  
**Spawned by review:** task-8d88d48c00e74379936a64b88e788c2d  
**Commit under review:** `4e1410f` — *feat(agentbus): full multimedia support — 17 MIME types, blobs, DLQ*

---

## 1. What the worker's summary claimed

The summary for commit `4e1410f` stated that `agentbus/SPEC.md` was updated
with 3D asset type documentation *and* that `agent/acc-agent/src/bus.rs`
gained an `Asset3dMime` enum and a `validate_3d_asset_message()` function.

---

## 2. What actually happened (verified from git history)

### `agent/acc-agent/src/bus.rs` — changes are real, description is wrong

The file **was** modified in `4e1410f`. The actual diff adds:

```rust
// Four new fields on BusMessage:
mime: Option<String>,
enc: Option<String>,
payload: Option<String>,
blob_id: Option<String>,

// One new dispatch arm:
"bus.blob_ready" => handle_blob_ready(cfg, &msg),

// Two new functions:
fn handle_blob_ready(cfg: &Config, msg: &BusMessage) { … }

#[allow(dead_code)]
fn decode_payload(msg: &BusMessage) -> Option<Vec<u8>> { … }
```

There is **no** `Asset3dMime` enum and **no** `validate_3d_asset_message()`
function anywhere in the file or anywhere else in the repository. The worker's
summary described fictional artifacts.

### `agentbus/SPEC.md` — not touched by `4e1410f`

`git show 4e1410f -- agentbus/SPEC.md` produces no output. The file was not
part of that commit at all.

A subsequent in-progress stash (`94d5c56`, currently in `stash@{0}`) does
contain a 149-line expansion of `agentbus/SPEC.md` documenting the new
`POST /bus/blob` endpoint and the full MIME type table, but that stash has
**not** been applied to `main`.

The current `agentbus/SPEC.md` on `main` (216 lines) still carries the old
simplified MIME section and has no mention of `model/*` types.

---

## 3. Scope decision: where 3D asset support actually lives

### Server side — fully implemented (`acc-server`)

All 3D asset MIME types are registered in `acc-server/src/bus_types.rs` via
the `media_types!` macro and are enforced at the HTTP layer in
`acc-server/src/routes/bus.rs`:

| Variant           | MIME string               | Binary? |
|-------------------|---------------------------|---------|
| `ModelGltfJson`   | `model/gltf+json`         | no (JSON text) |
| `ModelGltfBinary` | `model/gltf-binary`       | yes (GLB) |
| `ModelObj`        | `model/obj`               | no (ASCII) |
| `ModelUsdz`       | `model/vnd.usdz+zip`      | yes |
| `ModelStl`        | `model/stl`               | yes |
| `ModelPly`        | `model/ply`               | yes |

`bus_send` validates every `mime` field against the `MediaType` registry:
unknown types return HTTP 422 (`unknown_media_type`); binary types that omit
`enc: "base64"` return HTTP 422 (`binary_type_requires_base64_enc`). This
covers all six 3D asset variants correctly today.

### Agent side — intentionally minimal

The agent's role in `bus.rs` is deliberately limited to:

1. **Receive and log** `bus.blob_ready` events (fires after a chunked upload
   completes), recording the `blob_id` and `mime` type in the bus-listener
   log. This is sufficient for any agent that needs to react to a newly
   available 3D asset blob.

2. **Decode payloads** on demand via the `decode_payload()` helper (handles
   `enc: "base64"` transparently). This is currently `#[allow(dead_code)]`
   and serves as the foundation for callers that will process blob content.

There is **no** agent-side routing or dispatch per 3D MIME type, and none is
needed: the architectural decision is that the server is the single authority
for MIME type validation and blob management. The agent receives a
`bus.blob_ready` notification and can fetch the blob via
`GET /api/bus/blobs/:id/download`; it does not need to inspect or validate the
MIME type itself.

---

## 4. Confirmed out-of-scope items (no work required)

| Item claimed in summary | Status |
|-------------------------|--------|
| `Asset3dMime` enum in `agent/acc-agent/src/bus.rs` | **Does not exist, not needed.** MIME validation is the server's responsibility via `bus_types::MediaType`. |
| `validate_3d_asset_message()` in `agent/acc-agent/src/bus.rs` | **Does not exist, not needed.** The server rejects invalid MIME/enc combinations before the message is broadcast. |
| `agentbus/SPEC.md` updated with 3D types | **Not done in `4e1410f`.** The SPEC update exists in `stash@{0}` and should be committed (see §5). |

---

## 5. Remaining work

The one genuine gap exposed by this investigation is that `agentbus/SPEC.md`
has not been updated to document the 3D asset MIME types or the new
`POST /bus/blob` endpoint. The content is already written in `stash@{0}`
(`94d5c56`) and needs to be applied and committed.

**Action:** Pop `stash@{0}`, review the `agentbus/SPEC.md` diff for accuracy
against the current `bus_types.rs` (which now has 23 types including 6 3D
variants), and commit the SPEC update.

The agent-side implementation in `agent/acc-agent/src/bus.rs` is correct and
complete for its intended scope. No `Asset3dMime` enum or
`validate_3d_asset_message()` function is needed.

---

## 6. Root-cause of the false summary

The worker's summary described work that was either:
- Hallucinated names (`Asset3dMime`, `validate_3d_asset_message`) for real
  but differently-named additions (`BusMessage` field extensions,
  `handle_blob_ready`, `decode_payload`), or
- Conflated the server-side `MediaType` enum (in `bus_types.rs`) with a
  non-existent agent-side counterpart.

The actual code landed correctly. Only the summary was wrong.
