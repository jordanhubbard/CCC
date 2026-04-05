# agentOS Grafana Dashboard Setup

Live agentOS PD health dashboards: VibeEngine slots, GPU scheduler, watchdog misses, agent pool.

## Architecture

```
sparky (seL4/Microkit) → metrics_exporter PD → shared memory ring
                                ↓
                      CCC /api/agentos/metrics  ← Prometheus scrapes
                                ↓
                         Grafana dashboard
```

The `/api/agentos/metrics` endpoint (added to `rcc/api/index.mjs`) returns Prometheus text format
by aggregating data from `/api/agentos/slots` and `/api/mesh`. No direct seL4 scrape needed —
CCC is the bridge.

## Prometheus Scrape Config

Add to `/etc/prometheus/prometheus.yml` on do-host1:

```yaml
scrape_configs:
  # ... existing configs ...

  - job_name: 'agentos_sparky'
    scrape_interval: 30s
    scrape_timeout: 10s
    static_configs:
      - targets: ['127.0.0.1:8789']
        labels:
          host: 'sparky'
          agent: 'natasha'
    metrics_path: '/api/agentos/metrics'
    bearer_token: '<YOUR_RCC_TOKEN>'
```

Then reload Prometheus:
```bash
curl -X POST http://localhost:9090/-/reload
# or: systemctl reload prometheus / docker restart prometheus
```

## Grafana Dashboard Import

1. Open http://146.190.134.110:3000
2. Dashboards → Import → Upload JSON file
3. Select `rcc/deploy/grafana/agentOS-dashboard.json`
4. Select your Prometheus datasource

## Metrics Exposed

| Metric | Type | Description |
|--------|------|-------------|
| `agentos_vibe_slots_active` | gauge | Active VibeEngine WASM swap slots |
| `agentos_vibe_slots_idle` | gauge | Idle VibeEngine WASM swap slots |
| `agentos_vibe_slots_total` | gauge | Total slot capacity (max 4) |
| `agentos_gpu_queue_depth` | gauge | GPU scheduler pending task depth |
| `agentos_watchdog_miss_total` | counter | Cumulative watchdog heartbeat misses |
| `agentos_agent_pool_total` | gauge | Total agent pool worker slots (8) |
| `agentos_agent_pool_available` | gauge | Available agent pool workers |
| `agentos_slot_state` | gauge | Per-slot active/idle state (labelled by slot id) |
| `agentos_scrape_timestamp_seconds` | gauge | Unix timestamp of last scrape |

## Notes

- The metrics endpoint requires Bearer auth (same token as CCC queue API)
- Data is sourced from the `/api/agentos/slots` 30s cache — Prometheus scrape interval should be ≥30s
- `metrics_exporter.c` PD (commit a3a7e4f) is the seL4-side source; CCC is the HTTP bridge
