# CCC Hub — Docker Compose Deployment

Docker Compose is the recommended deployment path for AWS, Azure, and any fresh VPS.
It gets the full hub stack running in under 2 minutes.

## Prerequisites

- Docker Engine 24+ with the Compose plugin (`docker compose version`)
- A Linux VPS (Ubuntu 22.04 / Debian 12 recommended) with at least 1 vCPU / 512 MB RAM
- Port 8789 (API) and 8788 (dashboard) open in your firewall / security group

## Quick Start

```bash
# 1. Clone
git clone https://github.com/jordanhubbard/rockyandfriends
cd rockyandfriends/rcc-hub

# 2. Configure
cp .env.template .env
$EDITOR .env   # fill in RCC_PORT, CCC_AUTH_TOKENS, RCC_ADMIN_TOKEN at minimum

# 3. Launch
docker compose up -d

# 4. Verify
curl http://localhost:8789/health
# → {"status":"ok","uptime":...}
```

The hub will start and persist all data to named Docker volumes.

## Services

| Service         | Port  | Description                           |
|-----------------|-------|---------------------------------------|
| `rcc-api`       | 8789  | Node.js REST API + work queue         |
| `rcc-dashboard` | 8788  | Rust/WASM dashboard (serves frontend) |
| `minio`         | 9000/9001 | Optional S3 sidecar (see below)   |

## Configuration

All secrets live in `.env` — **never commit this file**.

The three required variables:

```dotenv
RCC_PORT=8789
CCC_AUTH_TOKENS=your-agent-token-1,your-agent-token-2
RCC_ADMIN_TOKEN=your-admin-token
```

Generate tokens with:

```bash
node -e "console.log(require('crypto').randomUUID())"
```

See `.env.template` for the full annotated list of options.

## Data Persistence

All state is stored in named Docker volumes:

| Volume           | Contents                                  |
|------------------|-------------------------------------------|
| `rcc-hub-data`   | Queue, agents, logs, brain state          |
| `rcc-minio-data` | MinIO objects (only when minio profile on)|

To back up data:

```bash
docker run --rm -v rcc-hub-data:/data -v $(pwd):/backup debian \
  tar czf /backup/rcc-data-$(date +%Y%m%d).tar.gz /data
```

## Optional: MinIO Sidecar

MinIO provides an S3-compatible store for model artifacts, large queue payloads, etc.

```bash
# Add MINIO_ROOT_PASSWORD to .env first
echo "MINIO_ROOT_PASSWORD=change-me-please" >> .env

# Start with MinIO
docker compose --profile minio up -d

# MinIO Console: http://your-server:9001
```

## Common Operations

```bash
# View logs
docker compose logs -f rcc-api

# Restart just the API
docker compose restart rcc-api

# Update to latest
git pull && docker compose up -d --build

# Stop everything
docker compose down

# Stop and wipe data volumes (destructive!)
docker compose down -v
```

## Building the Dashboard Binary

The `rcc-dashboard` service expects a pre-built Rust binary and WASM frontend.
Build on the host before deploying:

```bash
cd rcc/dashboard
make release
cp target/release/dashboard-server ../rcc-hub/dashboard-server
cp -r dist/ ../rcc-hub/dist/
```

If you're deploying on x86-64 Linux, you can also grab the pre-built binary from
[GitHub Releases](https://github.com/jordanhubbard/rockyandfriends/releases).

## Health Checks

Both services expose health endpoints:

```bash
curl http://localhost:8789/health   # API
curl http://localhost:8788/health   # Dashboard
```

Docker Compose will automatically restart unhealthy services after 3 failed checks.

## Next Steps

- **HTTPS / TLS**: See [HTTPS.md](HTTPS.md) to put Caddy or nginx in front for auto-TLS.
- **Getting Started**: See [GETTING-STARTED.md](GETTING-STARTED.md) for a full walkthrough.
- **Firewall**: Make sure ports 8789 and 8788 are open (or only 443/80 if using a reverse proxy).
