# acc-model

Shared domain types for the ACC fleet API, used by both `acc-server` and
`acc-client`.

Types are organized by resource and mirror the corresponding `/api/<resource>`
route groups on the server.  Only wire-facing representations live here —
server-internal state belongs in `acc-server`.

`acc-model` is a member of the root Cargo workspace alongside `acc-server`,
`acc-client`, `acc-cli`, and `agent/acc-agent`.  The Python
[`acc-client`](../clients/python/acc_client) package mirrors these types as
plain dicts with the same field names and `serde`-style renaming rules.

## Modules

| Module | Contents |
|--------|----------|
| `agent` | `Agent`, `AgentOnlineStatus` |
| `bus` | `BusMsg`, `BusSendRequest` |
| `error` | `ApiError` |
| `memory` | `MemorySearchRequest`, `MemoryStoreRequest`, `MemoryHit` |
| `project` | `Project`, `CreateProjectRequest`, `ProjectStatus` |
| `queue` | `QueueItem`, `ClaimItemRequest`, `CompleteItemRequest`, `FailItemRequest`, `CommentItemRequest`, `KeepaliveRequest`, `HeartbeatRequest` |
| `task` | `Task`, `TaskStatus`, `TaskType`, `ReviewResult`, `CreateTaskRequest`, `ClaimRequest`, `UnclaimRequest`, `CompleteRequest`, `ReviewResultRequest` |

## Design rules

- **Zero logic, zero network, zero async.** Pure `serde` structs anyone can
  depend on without pulling in Tokio or reqwest.
- **Unknown fields → `extra`.** Every top-level struct carries
  `#[serde(flatten)] pub extra: BTreeMap<String, Value>` so server additions
  don't break deserialization.
- **Server-internal shape stays in `acc-server`.** Richer status enums, audit
  fields, and query helpers live there; `acc-model` carries only what crosses
  the HTTP boundary.

## Usage

```toml
# Cargo.toml
[dependencies]
acc-model = { path = "../acc-model" }
```

```rust
use acc_model::{Task, TaskStatus, BusMsg, MemorySearchRequest};

// TaskStatus and TaskType implement FromStr via serde_json:
let status: TaskStatus = "in_progress".parse().unwrap();
```

## Type notes

### `BusMsg`

The wire field for message kind is `type` (a Rust keyword), mapped to the Rust
field `kind`.  The body is polymorphic — `Option<Value>` — because some
producers send a plain string and others send an embedded JSON object.

### `QueueItem`

`status` is `Option<String>` (not an enum) because the server emits both
`in-progress` and `in_progress`; keeping it as a string avoids deserialization
failures on future variants.

### `ReviewResultRequest`

`summary_hallucination: Option<bool>` — when `true`, signals that the worker's
output summary describes code that does not exist in the actual diff.  Recorded
by the server for analytics; reviewers set this via `acc-client`.
