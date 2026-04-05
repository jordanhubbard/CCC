# CCC Dashboard v2 — Architecture & Dev Setup

Replaces `dashboard/server.mjs` (Node.js, 2600+ lines) with a Rust/WASM stack
that eliminates the `process.env` browser leakage bug and is significantly
smaller and faster.

## Architecture

```
Browser
  └─ WASM (Leptos CSR) ←─ served by
       dashboard-server :8790
           ├── GET /api/*   ──proxy─→  rcc/api :8789  (auth injected server-side)
           ├── GET /bus/*   ──proxy─→  rcc/api :8789
           ├── GET /bus/stream  ─SSE─→ rcc/api :8789  (streamed, no timeout)
           └── /*           ──static─→ dist/  (WASM bundle + CSS)
```

**Key property:** `CCC_AGENT_TOKEN` never leaves the server. The browser only
sees the WASM binary and CSS. No Node.js, no `process.env`, no `require(`.

## Repo layout

```
rcc/dashboard/
  Cargo.toml               # workspace (server + ui)
  Makefile                 # build / dev / release targets
  dist/                    # trunk output (gitignored)
  dashboard-server/
    Cargo.toml
    src/main.rs            # axum HTTP server
  dashboard-ui/
    Cargo.toml
    Trunk.toml
    index.html
    style/main.css
    src/
      main.rs              # wasm entry point
      app.rs               # root App component
      types.rs             # shared data types (serde)
      components/
        agent_cards.rs     # online/offline, last heartbeat, host, model
        work_queue.rs      # filterable table, click-to-expand
        squirrelbus.rs     # SSE live message stream
        metrics.rs         # queue depth, completion rate, agent counts
        idea_incubator.rs  # incubating items + upvote action
```

## Config (environment variables)

| Variable             | Default                  | Notes                                      |
|---------------------|--------------------------|--------------------------------------------|
| `RCC_DASHBOARD_PORT`| `8790`                   | Change to 8788 after cutover               |
| `CCC_URL`           | `http://localhost:8789`  | Upstream CCC API address                   |
| `CCC_AGENT_TOKEN`   | _(empty)_                | Bearer token for CCC API auth              |
| `OPERATOR_HANDLE`   | `jkh`                    | Logged at startup                          |
| `DASHBOARD_DIST`    | `dist`                   | Path to built WASM bundle                  |

## Prerequisites

```sh
# Rust stable toolchain
rustup target add wasm32-unknown-unknown

# Trunk (WASM bundler)
cargo install trunk

# Optional: wasm-opt (smaller bundle)
# Ubuntu: apt install binaryen
# macOS:  brew install binaryen
```

## Building

```sh
cd rcc/dashboard

# Debug build
make build

# Release build (optimised WASM + server binary)
make release
```

Trunk outputs the WASM bundle + JS glue + CSS to `rcc/dashboard/dist/`.

## Running (dev)

```sh
cd rcc/dashboard

# Starts trunk watch (hot-reload) + Rust server side by side
make dev
```

Dashboard available at `http://localhost:8790`.

## Running (production)

```sh
cd rcc/dashboard
make release

RCC_DASHBOARD_PORT=8790 \
CCC_URL=http://localhost:8789 \
CCC_AGENT_TOKEN=<token> \
DASHBOARD_DIST=dist \
./target/release/dashboard-server
```

Or via systemd (see `deploy/systemd/ccc-dashboard.service`).

## Smoke tests

```sh
# Start the server first
make run &

# Run tests
TEST_PORT=8790 node rcc/tests/dashboard/smoke.test.mjs
```

## Migration / cutover plan

Dashboard v2 runs on port **8790** in parallel with the existing Node.js
dashboard on **8788**.

**Cutover steps:**

1. Build and start v2: `make release && make run-release`
2. Verify smoke tests pass: `TEST_PORT=8790 node rcc/tests/dashboard/smoke.test.mjs`
3. Do a manual review at `http://localhost:8790`
4. Stop the Node.js dashboard: `systemctl stop rcc-dashboard-legacy` (or kill process)
5. Change `RCC_DASHBOARD_PORT=8788` in the v2 systemd unit and restart
6. Update any bookmarks / nginx upstreams pointing to 8788

**Rollback:** just restart `dashboard/server.mjs` on 8788 — no data is
mutated by the dashboard.

## Cross-arch notes

The server binary targets the host architecture (x86_64 or aarch64). The
WASM bundle is architecture-neutral. To build for a different target:

```sh
cargo build -p dashboard-server --target aarch64-unknown-linux-gnu --release
```

The `dist/` folder produced by trunk can be copied as-is.

## Security properties

- Auth token is injected server-side only — never serialised into WASM or JS.
- The WASM bundle contains no secrets; it is safe to cache/CDN.
- CORS header `Access-Control-Allow-Origin: *` is set on proxied responses so
  dev tools can inspect them; tighten this in production if desired.
- Mutations (comment, choice, upvote, complete) require the server to be
  running with a valid `CCC_AGENT_TOKEN`.
