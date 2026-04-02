# Proposal: Shared File Store for Rocky, Bullwinkle & Natasha

## Problem

Currently only Bullwinkle can write to Google Drive (via `gog` as jkh). Rocky and Natasha must SCP files to Bullwinkle for uploads. This creates:
- **Single point of failure** — if Bullwinkle is down (like today's 13-hour budget outage), no Drive writes
- **Extra hops** — Rocky → SCP → Bullwinkle → gog → Drive is slow and fragile
- **SSH flakiness** — Rocky couldn't SSH to puck today (key mismatch)
- **No shared write surface** — the workqueue sync relies on chat messages, not a shared filesystem
- **Slow API** — Google Drive API through gog is not built for frequent small file operations

## Requirements

1. **All three agents can read AND write** — no single-writer bottleneck
2. **Works over Tailscale** — all nodes are on tail407856 tailnet
3. **Low latency** — queue state, handoffs, small files should be fast
4. **External publishing** — optional ability to share assets publicly (links, media)
5. **Eventual consistency** — works when 1-2 nodes are down, syncs when they come back
6. **Minimal setup** — jkh provides a token or runs one setup command

## Options Evaluated

### Option A: Tailscale + MinIO (S3-compatible) on Rocky
**What:** Run MinIO on do-host1 (CCC always-on hub). All agents use S3 API.
- ✅ All agents read/write via standard S3 SDK/CLI
- ✅ Rocky is always-on, so the store is always available
- ✅ S3 API is well-supported, fast, versioned
- ✅ Can generate presigned URLs for external sharing
- ✅ Buckets for different purposes (workqueue/, assets/, handoffs/)
- ✅ One token (access key + secret) for all agents
- ⚠️ Single server — if Rocky's VPS is down, store is down
- ⚠️ Needs ~50MB RAM for MinIO, negligible on DO droplet
- **Setup:** `apt install minio`, create access key, share with agents

### Option B: Syncthing mesh
**What:** Syncthing running on all three nodes, real-time file sync.
- ✅ True mesh — no single point of failure
- ✅ Real-time sync, works offline, syncs when reconnected
- ✅ Works great over Tailscale
- ✅ Perfect for the workqueue use case (sync queue.json automatically)
- ⚠️ No built-in external sharing/publishing
- ⚠️ Conflict resolution is file-level (last-write-wins or .sync-conflict files)
- ⚠️ Not great for large binary assets (syncs everything everywhere)
- **Setup:** Install on all three nodes, pair via device IDs, share folders

### Option C: Tailscale + rsync cron
**What:** Periodic rsync between nodes over Tailscale SSH.
- ✅ Dead simple, no new services
- ✅ Already works (we SCP today, just manually)
- ⚠️ Not real-time — depends on cron frequency
- ⚠️ SSH key management (already flaky today)
- ⚠️ Conflict resolution is messy
- ⚠️ No external sharing
- **Verdict:** This is what we have now but automated. Not a real improvement.

### Option D: Cloudflare R2 (managed S3)
**What:** Cloudflare's S3-compatible storage with free egress.
- ✅ All agents read/write via S3 API
- ✅ No server to maintain — fully managed
- ✅ Free egress + public bucket option for external sharing
- ✅ Works from anywhere (not Tailscale-dependent)
- ✅ Generous free tier (10GB storage, 10M reads, 1M writes/month)
- ⚠️ Requires Cloudflare account + API token
- ⚠️ Adds external dependency (but very reliable)
- **Setup:** Create R2 bucket, generate API token, share with agents

### Option E: Keep Google Drive + add service account
**What:** Create a GCP service account that all agents can use.
- ✅ No new infrastructure
- ✅ External sharing already works
- ⚠️ Google Drive API is still slow for frequent operations
- ⚠️ Service account setup is fiddly (GCP console, share folder with service account email)
- ⚠️ Rate limits are tight for Drive API
- **Verdict:** Fixes the auth problem but not the performance problem

## Recommendation

**Primary: Option A (MinIO on Rocky) + Option D (Cloudflare R2) as hybrid**

- **MinIO on CCC for internal collaboration — workqueue sync, handoffs, fast file exchange between agents. Always available (CCC hub, always-on), fast over Tailscale, S3 API.
- **Cloudflare R2** for external publishing — media assets, public links, anything shared outside the tailnet. Free egress, managed, reliable.
- **Google Drive** demoted to optional/archive — keep for existing shared folder, but stop using it as primary.

**Alternatively, if we want to minimize moving parts:**

**Option D alone (Cloudflare R2)** covers both internal and external needs. One bucket, one token, all three agents write. It's slightly higher latency than MinIO-on-tailnet for internal use, but simpler to manage.

## What jkh needs to do

### For MinIO (Option A):
1. SSH to Rocky: `ssh jkh@do-host1.tail407856.ts.net`
2. Install: `apt install minio` (or Docker: `docker run -p 9000:9000 minio/minio server /data`)
3. Create access key via `mc admin user add`
4. Share access key + secret with all three agents
5. Bind to Tailscale IP only (no public exposure needed)

### For Cloudflare R2 (Option D):
1. Log into Cloudflare dashboard
2. Create R2 bucket (e.g., `rocky-bullwinkle`)
3. Generate S3-compatible API token (read/write)
4. Share token with all three agents
5. Optionally enable public access for the `assets/` prefix

### For either:
- Estimated time: 15-20 minutes
- Ongoing cost: $0 (within free tiers)
- We handle all the agent-side config ourselves

## Migration Plan

1. Set up chosen solution
2. All agents add S3 credentials to their config
3. Move workqueue sync from chat-based to file-based (queue.json in shared bucket)
4. Test with a round-trip: Bullwinkle writes → Rocky reads → Natasha writes → Bullwinkle reads
5. Migrate handoffs/ from Google Drive
6. Keep Google Drive for legacy/archive
