# AgentFS — Content-Addressed WASM Module Store

Part of the CCC services layer. Owned by Natasha 🦊, runs on sparky.

## What It Does

Stores WASM modules by their SHA-256 hash. Same bytes = same hash = same module, always.
Modules are immutable once stored. Backend is MinIO (S3-compatible).

Integrates with the CCC vibe-swap demo: the vibe-swap slot requests a module by hash,
AgentFS streams it. Rocky/Bullwinkle can pull modules over Tailscale.

## API

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/agentfs/health` | none | Health check |
| POST | `/agentfs/modules` | Bearer | Upload WASM blob → `{hash, size}` |
| GET | `/agentfs/modules` | Bearer | List all stored modules |
| GET | `/agentfs/modules/:hash` | Bearer | Fetch WASM bytes (streams) |
| DELETE | `/agentfs/modules/:hash` | Bearer | Remove module + metadata |

## Usage

```bash
# Upload a module
curl -X POST http://sparky.tail407856.ts.net:8791/agentfs/modules \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/wasm" \
  --data-binary @mymodule.wasm

# Fetch by hash
curl http://sparky.tail407856.ts.net:8791/agentfs/modules/<sha256-hash> \
  -H "Authorization: Bearer <token>" \
  -o mymodule.wasm
```

## Validation

1. **Magic bytes** — checks `\0asm` + version 1 before accepting any blob
2. **Wasmtime AOT** — if `wasmtime` is on PATH, compiles to native for zero-latency hot-load on sparky

## Start (systemd)

```bash
sudo cp agentfs-natasha.service /etc/systemd/system/
sudo systemctl enable --now agentfs-natasha
```

## Config (env vars)

| Var | Default | Description |
|-----|---------|-------------|
| `AGENTFS_PORT` | `8791` | Listen port |
| `AGENTFS_TOKEN` | `agentfs-dev-token` | Bearer auth token |
| `MINIO_ENDPOINT` | `http://100.89.199.14:9000` | MinIO URL |
| `MINIO_ACCESS_KEY` | — | MinIO access key |
| `MINIO_SECRET_KEY` | — | MinIO secret key |
| `AGENTFS_BUCKET` | `agentfs-modules` | S3 bucket |
| `AGENTFS_MAX_MB` | `64` | Max upload size in MB |
| `WASMTIME_PATH` | `wasmtime` | Path to wasmtime binary (optional) |
