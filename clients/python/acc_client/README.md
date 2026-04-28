# acc-client (Python)

HTTP client for the ACC fleet API. Mirrors the Rust [`acc-client`](../../../acc-client/) crate in shape and behavior so callers can reason about both the same way.

## Install

```bash
pip install -e clients/python/acc_client
```

## Use

```python
from acc_client import Client

# From env / ~/.acc/.env:
c = Client.from_env()

# Or explicit:
c = Client(base_url="http://hub.local:8789", token="acc-...")

# Memory
hits = c.memory.search(query="buffer overflow in render pipeline", limit=10)
c.memory.store(text="Fixed NPE in agent.rs", metadata={"agent": "boris"})

# Tasks
open_tasks = c.tasks.list(status="open", task_type="work", limit=20)
task = c.tasks.claim(task_id="task-1", agent="boris")
c.tasks.complete(task_id="task-1", agent="boris", output="done")

# Heartbeat
c.items.heartbeat(agent="boris", status="ok", note="cycle 42")

# Bus — live SSE stream
for msg in c.bus.stream():
    # msg is a plain dict; loop ends when the server closes the connection.
    print(msg.get("type"), msg.get("from"), msg.get("body"))
```

## Token resolution

Precedence (highest first):

1. Explicit `Client(token=...)`
2. `ACC_TOKEN` env
3. `CCC_AGENT_TOKEN` env
4. `ACC_AGENT_TOKEN` env
5. `~/.acc/.env` (keys: `ACC_TOKEN`, `ACC_AGENT_TOKEN`)

Base URL defaults to `http://localhost:8789` and can be overridden via `ACC_URL`, `CCC_URL`, or `base_url=`.
