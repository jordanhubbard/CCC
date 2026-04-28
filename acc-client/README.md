# acc-client

Async Rust HTTP client for the ACC fleet API.

Depends on [`acc-model`](../acc-model) for shared wire types.  Mirrors the
Python [`acc-client`](../clients/python/acc_client) package in shape and
behavior so callers can reason about both the same way.

Both libraries belong to the same Cargo workspace (root `Cargo.toml`) which
also contains `acc-server` and `agent/acc-agent`, all sharing `acc-model` as
the single source of truth for wire types.

## Quick start

```toml
# Cargo.toml
[dependencies]
acc-client = { path = "../acc-client" }
```

```rust
use acc_client::{Client, model::TaskStatus};

#[tokio::main]
async fn main() -> Result<(), acc_client::Error> {
    // Resolve credentials from env / ~/.acc/.env (ACC_HUB_URL, ACC_TOKEN):
    let client = Client::from_env()?;

    // Tasks
    let tasks = client.tasks().list().status(TaskStatus::Open).send().await?;
    println!("{} open tasks", tasks.len());

    // Bus — live SSE stream
    use futures_util::StreamExt;
    let mut stream = client.bus().stream();
    while let Some(Ok(msg)) = stream.next().await {
        println!("{:?}", msg.kind);
    }

    Ok(())
}
```

## Token resolution

Precedence (highest first):

1. `Client::new(base_url, token)` — explicit
2. `ACC_TOKEN` environment variable
3. `~/.acc/.env` keys: `ACC_TOKEN`, then `ACC_AGENT_TOKEN`

Base URL for `Client::from_env()` comes from `ACC_HUB_URL`, defaulting to
`http://localhost:8789`.

## APIs

| Method | Description |
|--------|-------------|
| `client.tasks()` | CRUD + claim/unclaim/complete/review/vote |
| `client.projects()` | List / get / create / delete |
| `client.agents()` | Registry reads + name list |
| `client.queue()` | List / get queue items |
| `client.items()` | Claim / complete / fail / comment / keepalive / heartbeat |
| `client.bus()` | Send message, list recent, **SSE stream** |
| `client.memory()` | Semantic search + store |
| `client.request_json()` | Escape hatch for untyped endpoints |

## SSE bus stream

```rust
use acc_client::Client;
use futures_util::StreamExt;

let client = Client::from_env()?;
let stream = client.bus().stream();
tokio::pin!(stream);

while let Some(msg) = stream.next().await {
    let msg = msg?;
    println!("{}: {:?}", msg.kind.unwrap_or_default(), msg.from);
}
```

The stream yields `Result<BusMsg>` items.  Malformed JSON in a single frame is
silently skipped; the stream continues.  Reconnect logic lives at the call site.

## Error handling

```rust
use acc_client::{Client, Error};

match client.tasks().claim("task-9", "agent-a").await {
    Ok(task) => println!("claimed {}", task.id),
    Err(Error::Conflict(_)) => println!("already claimed"),
    Err(Error::Locked(body)) => {
        println!("blocked by {}", body.extra.get("pending").unwrap());
    }
    Err(e) => return Err(e.into()),
}
```

Typed variants: `Unauthorized` (401), `NotFound` (404), `Conflict` (409),
`Locked` (423), `AtCapacity` (429).  Everything else is `Api { status, body }`.

## Running the tests

```bash
cargo test -p acc-client
```

Integration tests in `tests/` spin up a [wiremock](https://docs.rs/wiremock)
server and exercise the client against canned responses that mirror the real
server's wire format.
