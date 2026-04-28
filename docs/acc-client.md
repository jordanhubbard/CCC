# ACC Shared Client Libraries — Developer Guide

**Epic:** Extract shared ACC client libraries (Rust + Python)  
**Status:** Implemented in `acc-model` / `acc-client` (Rust) and `clients/python/acc_client/` (Python)

---

## Overview

The shared client libraries eliminate HTTP-client drift between `acc-cli`,
`acc-agent`, and the hermes `acc_shared_memory` plugin.  Every consumer of the
ACC fleet API now goes through the same transport layer with identical wire
semantics.

Two parallel implementations exist, identical in shape and behaviour:

| Language | Location | Package / crate | Transport |
|----------|----------|-----------------|-----------|
| **Rust** | `acc-client/` | `acc-client` crate | async `reqwest` |
| **Python** | `clients/python/acc_client/` | `acc-client` PyPI package | sync `httpx` |

Shared wire types for the Rust side live in the `acc-model` crate.

---

## Architecture decisions (locked)

1. **`acc-model`** — Zero-async, zero-network crate.  Pure `serde` structs
   shared by `acc-server`, `acc-client`, `acc-cli`, and `acc-agent`.  The single
   source of truth for `BusMsg`, `Task`, `QueueItem`, `Agent`, `MemoryHit`, etc.
2. **SSE bus streaming in v1** — Both Rust (`BusApi::stream()`) and Python
   (`Client.bus.stream()`) ship a live `GET /api/bus/stream` SSE consumer.
3. **Python package location** — `clients/python/acc_client/` inside this repo;
   no separate repository.
4. **Cargo workspace** — `acc-cli`, `acc-server`, `agent/acc-agent`, `acc-model`,
   and `acc-client` all live in the root workspace (`Cargo.toml` at repo root).

---

## Cargo workspace

The root `Cargo.toml` declares all five Rust crates as workspace members:

```
acc-model        acc-model/
acc-client       acc-client/
acc-cli          acc-cli/
acc-server       acc-server/
acc-agent        agent/acc-agent/
```

Run the full Rust test suite from the repo root:

```bash
cargo test --workspace
# or via make:
make test
```

Shared dependency versions are pinned in `[workspace.dependencies]` and opted
into by member crates with `dep.workspace = true`.

---

## `acc-model` crate

**Path:** `acc-model/`  
**Crate name:** `acc_model`

Pure data layer.  No networking, no async, no Tokio.  Every type that crosses
the HTTP boundary lives here.

### Modules

| Module | Key types |
|--------|-----------|
| `agent` | `Agent`, `AgentOnlineStatus` |
| `bus` | `BusMsg`, `BusSendRequest` |
| `error` | `ApiError` |
| `memory` | `MemorySearchRequest`, `MemoryStoreRequest`, `MemoryHit` |
| `project` | `Project`, `CreateProjectRequest`, `ProjectStatus` |
| `queue` | `QueueItem`, `ClaimItemRequest`, `CompleteItemRequest`, `FailItemRequest`, `CommentItemRequest`, `KeepaliveRequest`, `HeartbeatRequest` |
| `task` | `Task`, `TaskStatus`, `TaskType`, `ReviewResult`, `CreateTaskRequest`, `ClaimRequest`, `UnclaimRequest`, `CompleteRequest`, `ReviewResultRequest` |

### Design rules

- **Unknown fields → `extra`.**  Every top-level struct carries
  `#[serde(flatten)] pub extra: BTreeMap<String, Value>` so server additions
  don't break deserialization.
- **Wire naming quirks are handled here.** `BusMsg::kind` maps to the wire
  field `"type"`.  `Agent::agent_type` maps to `"type"`.  `Agent::registered_at`
  maps to `"registeredAt"`.
- **Minimal enums.** Status fields that have historically shipped with both
  hyphen and underscore spellings (e.g. `in-progress` vs `in_progress`) are
  kept as `Option<String>` in `QueueItem` to avoid deserialization failures.

### Adding a new field

1. Add the field to the relevant `acc-model` struct.
2. If it's a new top-level struct: re-export it from `acc-model/src/lib.rs`.
3. Add a round-trip unit test in the same module file.

---

## `acc-client` crate

**Path:** `acc-client/`  
**Crate name:** `acc_client`

Async HTTP client built on `reqwest`.  Exposes `acc_model` types under the
`acc_client::model` re-export alias so callers only need one import.

### Quick start

```toml
# Cargo.toml
[dependencies]
acc-client = { path = "../acc-client" }
```

```rust
use acc_client::{Client, model::TaskStatus};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> acc_client::Result<()> {
    let client = Client::from_env()?;

    // Tasks
    let tasks = client.tasks().list().status(TaskStatus::Open).send().await?;

    // SSE bus stream
    let stream = client.bus().stream();
    tokio::pin!(stream);
    while let Some(Ok(msg)) = stream.next().await {
        println!("{:?}", msg.kind);
    }
    Ok(())
}
```

### Sub-API surface

| Method | Type | Description |
|--------|------|-------------|
| `client.tasks()` | `TasksApi` | CRUD + claim/unclaim/complete/review/vote |
| `client.projects()` | `ProjectsApi` | List / get / create / delete |
| `client.agents()` | `AgentsApi` | Registry reads + name list |
| `client.queue()` | `QueueApi` | List / get queue items |
| `client.items()` | `ItemsApi` | Claim / complete / fail / comment / keepalive / heartbeat |
| `client.bus()` | `BusApi` | Send message, list recent, **SSE stream** |
| `client.memory()` | `MemoryApi` | Semantic search + store |
| `client.request_json()` | — | Escape hatch for untyped endpoints |

### Token resolution

Precedence (highest first):

1. `Client::new(base_url, token)` — explicit
2. `ACC_TOKEN` environment variable
3. `~/.acc/.env` keys: `ACC_TOKEN`, then `ACC_AGENT_TOKEN`

Base URL for `Client::from_env()` comes from `ACC_HUB_URL`, defaulting to
`http://localhost:8789`.

### Error handling

```rust
use acc_client::{Client, Error};

match client.tasks().claim("task-9", "agent-a").await {
    Ok(task)               => println!("claimed {}", task.id),
    Err(Error::Conflict(_)) => println!("already claimed"),
    Err(Error::Locked(b))   => println!("blocked by {:?}", b.extra.get("pending")),
    Err(e)                  => return Err(e.into()),
}
```

Typed variants: `Unauthorized` (401), `NotFound` (404), `Conflict` (409),
`Locked` (423), `AtCapacity` (429).

### SSE streaming

`BusApi::stream()` returns `impl Stream<Item = Result<BusMsg>>`.

- Malformed JSON in a single frame is silently skipped; the stream continues.
- Keep-alive comment frames (`: keepalive`) are transparently discarded.
- The stream terminates cleanly when the server closes the connection.
- Reconnect logic is the caller's responsibility.

### Running the tests

```bash
cargo test -p acc-client           # unit + integration (wiremock)
cargo test -p acc-model            # model-level unit tests
cargo test --workspace             # everything
```

---

## Python `acc-client` package

**Path:** `clients/python/acc_client/`  
**PyPI name (when published):** `acc-client`

Synchronous HTTP client built on `httpx`.  Mirrors the Rust crate in shape
and wire behaviour.

### Quick start

```bash
pip install -e clients/python/acc_client
# or with test extras:
pip install -e "clients/python/acc_client[test]"
```

```python
from acc_client import Client

with Client.from_env() as c:
    # Tasks
    tasks = c.tasks.list(status="open", limit=20)
    task  = c.tasks.claim("task-abc", agent="natasha")
    c.tasks.complete("task-abc", agent="natasha", output="done")

    # Review with hallucination flag
    c.tasks.review_result(
        "task-abc",
        result="rejected",
        agent="reviewer",
        summary_hallucination=True,
    )

    # Memory (hermes acc_shared_memory plugin)
    hits = c.memory.search("buffer overflow in render pipeline", limit=10)
    c.memory.store("Fixed NPE in agent.rs", metadata={"agent": "boris"})

    # Bus
    c.bus.send("tasks:added", from_="boris", body="new item queued")
    for msg in c.bus.stream():          # live SSE — blocks until server closes
        print(msg.get("type"), msg.get("from"))
```

### Sub-API surface

| Attribute | Class | Rust analogue |
|-----------|-------|---------------|
| `client.tasks` | `TasksApi` | `acc_client::tasks` |
| `client.projects` | `ProjectsApi` | `acc_client::projects` |
| `client.queue` | `QueueApi` | `acc_client::queue` |
| `client.items` | `ItemsApi` | `acc_client::items` |
| `client.bus` | `BusApi` | `acc_client::bus` |
| `client.memory` | `MemoryApi` | `acc_client::memory` |
| `client.agents` | `AgentsApi` | `acc_client::agents` |

### Token resolution

Precedence (highest first):

1. `Client(token="…")` — explicit
2. `ACC_TOKEN` environment variable
3. `CCC_AGENT_TOKEN` environment variable (legacy)
4. `ACC_AGENT_TOKEN` environment variable
5. `~/.acc/.env` keys: `ACC_TOKEN`, then `ACC_AGENT_TOKEN`

Base URL: `ACC_HUB_URL` → `ACC_URL` → `CCC_URL` → `http://localhost:8789`.

### Error handling

```python
from acc_client import Client, Conflict, Locked, NotFound

with Client.from_env() as c:
    try:
        c.tasks.claim("task-9", agent="boris")
    except Conflict as e:
        print("already claimed:", e.code)
    except Locked as e:
        print("blocked by:", e.extra.get("pending"))
    except NotFound:
        print("task vanished")
```

Exception hierarchy: `NoToken`, `ApiError` → `Unauthorized`, `NotFound`,
`Conflict`, `Locked`, `AtCapacity`.

### `bus.send()` — the `from_` convention

The Python keyword `from` is reserved, so the wire field `"from"` is exposed
as the `from_` keyword argument:

```python
client.bus.send("tasks:added", from_="boris", body="queued item")
# wire body: {"type": "tasks:added", "from": "boris", "body": "queued item"}
```

### Running the tests

```bash
cd clients/python/acc_client
pip install -e ".[test]"
pytest                   # all tests
pytest -q --tb=short     # compact output (CI-style)
```

The test suite uses [respx](https://lundberg.github.io/respx/) for HTTP
mocking — no live server required.

```bash
# or via make from the repo root:
make test-python
```

---

## Wire compatibility notes

Both implementations follow the same conventions:

- `BusMsg.kind` / `msg["type"]` — the wire field is `"type"`.
- `Agent.agent_type` / `agent["type"]` — wire field is `"type"`.
- camelCase fields (`registeredAt`, `claimedBy`, `onlineStatus`, …) are
  accepted verbatim in Python (dict keys) and renamed in Rust structs with
  `#[serde(rename)]`.
- Unknown fields are preserved: Rust structs carry a `BTreeMap<String, Value>`
  `extra` field; Python returns raw dicts so all server fields pass through.
- Both clients accept both envelope shapes from the server (e.g.
  `{"tasks": [...]}` and a bare `[...]` array).

---

## Adding a new endpoint

1. Add request/response types to `acc-model` (if they don't exist).
2. Add the method to the appropriate `acc-client` Rust sub-API module.
3. Add a wiremock-based integration test in `acc-client/tests/`.
4. Mirror the method in the Python `_<resource>.py` module.
5. Add a respx-based test in `clients/python/acc_client/tests/`.
