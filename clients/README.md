# clients — ACC shared client libraries

This directory contains the client libraries extracted by the
**Epic: shared ACC client libraries (Rust + Python)**.

Two parallel implementations, identical in shape and wire behaviour:

| Language | Location | Package / crate | Description |
|----------|----------|-----------------|-------------|
| **Rust** | [`acc-client/`](../acc-client/) | `acc-client` crate | Async `reqwest`-based client; part of the root Cargo workspace |
| **Python** | [`python/acc_client/`](python/acc_client/) | `acc-client` PyPI package | Synchronous `httpx`-based client; mirrors the Rust crate shape |

Shared wire types for the Rust side live in the
[`acc-model`](../acc-model/) crate.

---

## Design decisions (locked)

1. **`acc-model`** — zero-async, zero-network crate for all wire types shared
   between `acc-server`, `acc-client`, `acc-cli`, and `acc-agent`.
2. **SSE bus streaming in v1** — both Rust (`BusApi::stream()`) and Python
   (`Client.bus.stream()`) ship a live `GET /api/bus/stream` SSE consumer.
3. **Python package location** — `clients/python/acc_client/` inside this
   repo; no separate repository.
4. **Cargo workspace** — `acc-cli`, `acc-server`, `agent/acc-agent`,
   `acc-model`, and `acc-client` all live in the root workspace
   (`Cargo.toml` at repo root).

---

## Quick start

### Rust

```toml
# Cargo.toml — already in the workspace, just add the dep:
[dependencies]
acc-client = { path = "../acc-client" }
acc-model  = { path = "../acc-model" }
```

```rust
use acc_client::Client;
use futures_util::StreamExt;

let client = Client::from_env()?;
let tasks = client.tasks().list().send().await?;

// SSE bus stream
let stream = client.bus().stream();
tokio::pin!(stream);
while let Some(Ok(msg)) = stream.next().await {
    println!("{:?}", msg.kind);
}
```

### Python

```bash
pip install -e clients/python/acc_client
# or with test extras:
pip install -e "clients/python/acc_client[test]"
```

```python
from acc_client import Client

with Client.from_env() as c:
    tasks = c.tasks.list(status="open")
    for msg in c.bus.stream():        # live SSE
        print(msg.get("type"))
```

---

## Running the tests

```bash
# Rust (all workspace crates):
cargo test --workspace
# or via make:
make test

# Python:
make test-python
# or directly:
cd clients/python/acc_client && pytest
```

The Python suite uses [respx](https://lundberg.github.io/respx/) to mock HTTP
— no live server required.
