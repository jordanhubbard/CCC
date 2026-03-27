# Storage Topology Recommendation
**Item:** wq-R-019  
**Author:** Rocky  
**Date:** 2026-03-21  
**Status:** Draft — awaiting jkh decision

---

## Problem Summary

| Agent | Tailscale | MinIO (private) | Azure Blob (public) |
|-------|-----------|-----------------|---------------------|
| Rocky | ✅ | ✅ | ✅ |
| Bullwinkle | ✅ | ✅ | ✅ |
| Natasha | ✅ | ✅ | ✅ |
| Boris | ❌ | ❌ (no Tailscale) | ✅ |

**Gap:** Boris can't reach the private tier. Azure Blob is public-only — not suitable for intermediate artifacts, internal agent state, or unpublished renders. Boris needs a private working store.

---

## Option Analysis

### Option 1: Azure Blob with Private Containers (Shared Key + Container ACL)
**How:** Create a new container `private-agents` in the existing Azure storage account. Use Shared Key auth (access key) for all agents. Set container access to Private (no public blob listing). All 4 agents authenticate with the storage key and read/write directly.

**Pros:**
- Zero new infrastructure — uses existing Azure account
- Boris already has Azure access → zero new config for him
- Shared Key auth works from any host, no VPN required
- Can scope per-agent with separate containers or blob prefixes
- Current SAS token on `assets/` stays public → publishing layer unchanged
- Private container contents are NOT publicly accessible (require auth)

**Cons:**
- Shared Key is more permissive than ideal — any agent with the key can read/write any blob in the account (mitigated with separate containers)
- No IP allowlisting at the storage account level without Azure networking config
- Slightly higher Azure egress costs vs MinIO (negligible at our scale)

**Implementation:** Create `agents-private` container, generate a scoped SAS token (rwdl, no public access, long expiry) or share the storage account key. Update all agents' configs. Boris gets the same key.

**Cost:** ~$0.02/GB/month storage. Reads/writes at Azure blob pricing (trivial at our volume). Zero new infra.

---

### Option 2: Rocky as S3 Proxy (HTTPS Reverse Proxy to MinIO)
**How:** Deploy a small authenticated HTTPS proxy on the hub node (nginx or Caddy) that forwards S3 API calls to MinIO at localhost:9000. Expose it at a public endpoint (e.g., `https://s3.the hub node.example.com`). All agents including Boris hit the proxy with a strong bearer token.

**Pros:**
- Keeps all data on MinIO (our proven, already-live store)
- Fine-grained auth — proxy validates token before forwarding to MinIO
- No Azure dependency for private storage
- Boris gets access without Tailscale

**Cons:**
- New infra to deploy and maintain on the hub node
- Single point of failure: if the hub node goes down, all agents lose private storage
- Proxy adds latency for all agents (Rocky, Bullwinkle, Natasha currently hit MinIO directly at low latency)
- TLS cert management needed
- Boris's GPU render artifacts are large → routing everything through the hub node proxy is inefficient

**Cost:** Compute already sunk (the hub node running). Egress costs for proxy routing. SSL cert (free via Let's Encrypt).

---

### Option 3: Azure Container Instance Running MinIO
**How:** Deploy a MinIO instance in Azure (ACI or VM). Connect via Azure VNet with private endpoint. All agents access via Azure private IP or hostname.

**Pros:**
- Full S3-compatible API, private by design
- Boris native (same Azure cloud, potentially low-latency)

**Cons:**
- Additional monthly cost ($15-40/mo for ACI with storage)
- New infrastructure to operate and monitor
- Doesn't solve the problem of "Rocky/Bullwinkle/Natasha already have a working MinIO" — creates a second private store to sync
- Azure VNet private endpoints don't help Rocky/Bullwinkle/Natasha (not in Azure VNet)
- Overkill for our current scale

**Cost:** ~$15-40/month + storage. Not recommended.

---

### Option 4: Tailscale Exit Node on Rocky (Boris tunnels through)
**How:** Configure Rocky as a Tailscale subnet router / exit node. Boris connects through Rocky's Tailscale node to reach MinIO at 100.89.199.14.

**Pros:**
- Boris gets full MinIO access without changing storage architecture
- Single source of truth: one MinIO, one schema, one set of bucket policies

**Cons:**
- Boris's container environment may not support Tailscale (it's the reason he lacks it now)
- Adds routing complexity — Boris's MinIO traffic routes through Rocky (latency, dependency)
- If Boris is containerized (likely) and Tailscale requires kernel-level networking, this may be blocked by container runtime
- Exit node setup on Rocky requires kernel IP forwarding config changes

**Likely blocked:** Boris's lack of Tailscale is almost certainly a container runtime constraint, not a config oversight. Exit-node routing doesn't bypass container network namespacing.

---

## Recommendation: Option 1 (Azure Blob Private Container)

**Rationale:**

1. **Zero new infra.** We already have an Azure storage account. A second container is one CLI command.
2. **Boris gets access immediately.** No config changes on his side beyond adding the storage key — he already knows how to write to Azure.
3. **Clean tier separation:**
   - `assets/` container: public, internet-readable, SAS token (current — unchanged)
   - `agents-private/` container: private, Shared Key auth, not publicly accessible
4. **MinIO stays for Tailscale-native agents.** Rocky, Bullwinkle, Natasha continue using MinIO at its current low-latency Tailscale endpoint for internal coordination data (heartbeats, syncLog, peer-status, etc.). No migration needed.
5. **Boris-specific data flows to Azure private.** His render outputs, intermediate artifacts, and agent state go to `agents-private/`. All four agents can read his outputs without Tailscale.

---

## Proposed Storage Tiers (Post-Implementation)

| Tier | Backend | Who | Access | Use Case |
|------|---------|-----|--------|----------|
| **Public publish** | Azure Blob `assets/` | All | Public URL | Dashboard HTML, published assets, public files |
| **Private working** | Azure Blob `agents-private/` | All | Shared Key or scoped SAS | Boris render outputs, cross-agent artifacts requiring Boris access |
| **Internal coordination** | MinIO `agents/shared/` | Rocky/Bullwinkle/Natasha | Tailscale only | Heartbeats, jkh-state, syncLog, peer-status, health metrics |

**Boris-specific note:** Boris uses Azure private for everything (no MinIO access). Coordination data he needs (wq sync, peer-status reads) should be mirrored to Azure private by Rocky periodically, or Boris should have a read endpoint for relevant MinIO files exposed via a lightweight sync script.

---

## Implementation Plan (if approved)

1. **Rocky:** `az storage container create --name agents-private --public-access off` (or `mc mb` equivalent via Azure CLI)
2. **Generate scoped SAS token** for `agents-private` with permissions: read, write, delete, list — no public access. Expiry: 2030. Store in TOOLS.md / agent configs.
3. **Update Boris's config** with the new container URL + SAS token.
4. **Document in TOOLS.md** under Storage section alongside existing Azure Blob entry.
5. **Optional:** Script to mirror key MinIO shared files (peer-status.json, agent-heartbeats) to Azure private so Boris has full visibility without a second tier dependency.

---

## Open Questions for jkh

1. **Approve Option 1 (Azure private container)?** Rocky can implement immediately.
2. **Separate SAS per agent or shared key?** Shared key is simpler; per-agent SAS is cleaner for audit.
3. **Should Boris read MinIO coordination data via a periodic mirror, or is Azure-only coordination sufficient?** (He already checks the dashboard at <your-dashboard-url> — that covers heartbeats.)

---

*Analysis by Rocky, 2026-03-21T16:00Z*
