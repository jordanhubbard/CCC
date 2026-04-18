# Consul Service Discovery — Historical

> **Status: NOT IN USE.** Consul was installed via migrations 0009/0010 but was never
> successfully running across the fleet. Migration 0014 (`cleanup_legacy_and_consul`)
> stops and disables any Consul instances still running. All services now use
> `localhost` addresses directly — no service mesh or DNS resolution is required.

This directory and these scripts are retained as a historical record of the consul
installation attempt. They are not needed for normal CCC operation.

## What Was Planned

CCC was designed to use Consul for internal service mesh so that no IP addresses
would appear in source code. Services would register as `<name>.service.consul`.

## What Replaced It

All services use `localhost` with their standard ports:

| Service     | Port  | Used via             |
|-------------|-------|----------------------|
| acc-server  | 8789  | `CCC_URL` in .env    |
| tokenhub    | 8090  | `TOKENHUB_URL` in .env |
| qdrant      | 6333  | `QDRANT_URL` in .env |
| minio       | 9000  | `MINIO_ENDPOINT` in .env |
| ollama      | 11434 | direct localhost     |

For agents on different hosts, `CCC_URL` is set to the hub's public or Tailscale URL.
The optional `CCC_TAILSCALE_URL` env var provides automatic failover via
`ccc-connectivity-check.sh`.

## Directory Layout

```
deploy/consul/
├── README.md          # This file
├── consul.hcl.tmpl    # Config template (rendered by migration 0009 — historical)
└── service-defs/      # Agent service registrations (historical)
    ├── rocky.hcl
    ├── natasha.hcl
    └── bullwinkle.hcl
```
