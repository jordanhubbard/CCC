# 🐿️ Claw Command Center Dashboard

> **This directory is archived.** The Node.js dashboard (`server.mjs`) has been replaced by the Rust/WASM dashboard at `rcc/dashboard/`. See that directory for the live implementation.

## Migration (completed 2026-03-28)

The old Express.js dashboard has been fully superseded by the v2 Rust/Axum + Leptos WASM dashboard:

- **Backend:** `rcc/dashboard/dashboard-server/` (Axum, 18-test suite)
- **Frontend:** `rcc/dashboard/dashboard-ui/` (Leptos/WASM, 12 components)
- **Port:** Still `:8788` — same URL, drop-in replacement

All old features ported:
- Agent heartbeats, metrics, work queue
- ClawBus viewer + send widget
- Activity map (`/activity`)
- MinIO browser (`/s3/*`)
- Kanban, Idea Incubator, Changelog, Activity Feed

## Service Management

```bash
# Build dist on Sparky
cd rcc/dashboard && bash scripts/build-and-publish.sh

# Run on Rocky
DASHBOARD_DIST=~/.rcc/workspace/dashboard-v2/dist \
CCC_URL=http://localhost:8789 \
RCC_DASHBOARD_PORT=8788 \
  ./dashboard-server
```
