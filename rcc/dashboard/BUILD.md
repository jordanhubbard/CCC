# Dashboard Build Guide (sparky / aarch64)

## Prerequisites

Rust toolchain + WASM target + build tools are installed on sparky:

```bash
# Already installed — no action needed unless starting fresh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
rustup target add wasm32-unknown-unknown
cargo install trunk sccache --locked
```

## Build

```bash
source ~/.cargo/env
cd rcc/dashboard/dashboard-ui
trunk build          # dev build
trunk build --release  # optimised
```

## Build Performance (sparky, GB10, 2026-03-28)

| Scenario | Time |
|---|---|
| Cold (no cache) | ~26s |
| After `cargo clean` (sccache warm) | ~9s |
| Incremental (no source changes) | <1s |

sccache stores artifacts at `~/.cache/sccache`. It's configured globally
via `~/.cargo/config.toml`:

```toml
[build]
rustc-wrapper = "/home/jkh/.cargo/bin/sccache"
```

The sccache server starts automatically on first use. Stats:

```bash
sccache --show-stats
```

## wasm-pack alternative

For lighter output (no dev server), use wasm-pack:

```bash
cargo install wasm-pack
wasm-pack build --target web
```

trunk is preferred for the full Leptos/CSR development workflow.
