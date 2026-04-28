# clients/python

Python client libraries for the ACC fleet API.

## Packages

| Directory | PyPI name | Description |
|-----------|-----------|-------------|
| [`acc_client/`](acc_client/) | `acc-client` | Synchronous HTTP client — tasks, projects, queue, agents, bus SSE, memory |

## Quick start

```bash
# Install (editable) from this repo root:
pip install -e clients/python/acc_client

# Or in a virtualenv alongside other dev deps:
pip install -e "clients/python/acc_client[test]"
```

## Usage

```python
from acc_client import Client

# Auto-resolve credentials from env / ~/.acc/.env:
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
        summary_hallucination=True,   # summary doesn't match the actual diff
    )

    # Idea vote
    c.tasks.vote("task-xyz", agent="boris", vote="approve", refinement="scope A only")

    # Memory (hermes acc_shared_memory plugin path)
    hits = c.memory.search("buffer overflow in render pipeline", limit=10)
    c.memory.store("Fixed NPE in agent.rs", metadata={"agent": "boris"})

    # Bus — send a message
    c.bus.send("tasks:added", from_="boris", body="new item enqueued")

    # Bus — live SSE stream (blocks until server closes connection)
    for msg in c.bus.stream():
        print(msg.get("type"), msg.get("from"), msg.get("body"))
```

## Token resolution

Precedence (highest first):

1. `Client(token="...")`
2. `ACC_TOKEN` environment variable
3. `CCC_AGENT_TOKEN` environment variable (legacy)
4. `ACC_AGENT_TOKEN` environment variable
5. `~/.acc/.env` keys: `ACC_TOKEN`, then `ACC_AGENT_TOKEN`

Base URL defaults to `http://localhost:8789`; override via `ACC_HUB_URL`,
`ACC_URL`, `CCC_URL`, or `Client(base_url="...")`.

## Error handling

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

Typed exceptions: `Unauthorized` (401), `NotFound` (404), `Conflict` (409),
`Locked` (423), `AtCapacity` (429).  All inherit from `ApiError`.

## Running the tests

```bash
cd clients/python/acc_client
pip install -e ".[test]"
pytest
```

The test suite uses [respx](https://lundberg.github.io/respx/) to mock HTTP
calls — no live server required.
