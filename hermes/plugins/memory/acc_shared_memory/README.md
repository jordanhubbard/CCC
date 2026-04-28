# acc_shared_memory

Hermes memory provider plugin that stores and retrieves agent memories via
the **ACC hub** (`/api/memory/search` and `/api/memory/store`).

This is one of the deliverables from the
**Epic: extract shared ACC client libraries (Rust + Python)** and is the
canonical consumer of the Python `acc_client` package
(`clients/python/acc_client/`) inside hermes.

---

## Why this plugin exists

Before this epic, the hermes `acc_shared_memory` feature was implemented with
an ad-hoc `requests` call that duplicated the auth and serialisation logic
already present in `acc-cli` and `acc-agent`.  The refactor:

1. Extracts `acc-model` (shared Rust wire types) and `acc-client` (async Rust
   HTTP client + Python mirror) as first-class crates/packages.
2. **This plugin** is the Python side's canonical consumer — it routes all
   memory I/O through `acc_client.Client.memory`, which is a thin wrapper
   around the same `/api/memory/*` endpoints as the Rust crate.

---

## Activation

```yaml
# ~/.config/hermes/config.yaml  (or the active HERMES_HOME/config.yaml)
memory:
  provider: acc_shared_memory
  collection: hermes          # optional; the Qdrant collection name
  search_limit: 10            # optional; max hits per recall
```

Credentials (highest priority first):

| Source | Key |
|--------|-----|
| Environment variable | `ACC_TOKEN` |
| `~/.acc/.env` file | `ACC_TOKEN` |
| Environment variable (legacy) | `CCC_AGENT_TOKEN` / `ACC_AGENT_TOKEN` |

Base URL defaults to `http://localhost:8789`; override with `ACC_HUB_URL`.

---

## What it does

| Lifecycle hook | Behaviour |
|---|---|
| `is_available()` | Returns `True` if `acc_client` is importable **and** a token is resolvable |
| `initialize(session_id, **kw)` | Opens the `httpx` connection pool; reads `collection` / `search_limit` from kwargs |
| `system_prompt_block()` | Injects a one-paragraph note about the shared memory backend |
| `prefetch(query)` | Calls `POST /api/memory/search`; returns a formatted hit list for the context window |
| `sync_turn(user, asst)` | **No-op** — writes are explicit via tool calls |
| `get_tool_schemas()` | Exposes `acc_memory_search` and `acc_memory_store` to the model |
| `handle_tool_call(name, inp)` | Dispatches to `client.memory.search()` or `client.memory.store()` |
| `on_memory_write(action, target, content)` | Mirrors built-in MEMORY.md writes to the ACC hub |
| `on_session_end(messages)` | Closes the HTTP connection pool cleanly |
| `shutdown()` | Alias for `on_session_end` |

---

## Tools exposed to the model

### `acc_memory_search`

```json
{
  "query": "buffer overflow in render pipeline",
  "limit": 10,
  "collection": "hermes"
}
```

Returns `{ "hits": [...], "count": N }`.  Each hit is a `MemoryHit` dict
with `text`, `score`, and optional `metadata` / `id` fields.

### `acc_memory_store`

```json
{
  "text": "Fixed NPE in agent.rs line 42",
  "metadata": { "agent": "boris", "task_id": "task-123" },
  "collection": "hermes"
}
```

Returns `{ "ok": true, "stored": <char count> }`.

---

## Wire compatibility

The plugin uses `acc_client.Client.memory` which is the Python mirror of the
Rust `acc_client::memory` module.  Both map to the same two endpoints and use
the same `acc-model` wire shapes:

| Python type | Rust type | Wire JSON |
|---|---|---|
| `dict {"query", "limit?", "collection?"}` | `MemorySearchRequest` | `POST /api/memory/search` body |
| `dict {"text", "metadata?", "collection?"}` | `MemoryStoreRequest` | `POST /api/memory/store` body |
| `list[dict]` | `Vec<MemoryHit>` | response body |

---

## Running the tests

```bash
# Python unit tests (no live server required — uses respx mocks):
cd clients/python/acc_client && pytest -q

# Or via make:
make test-python
```

The plugin itself is tested indirectly through the `acc_client` test suite.
A dedicated plugin test lives at
`hermes/plugins/memory/acc_shared_memory/tests/` (future).
