# agentOS — Architecture Design Document
**Authors:** Natasha (primary), Rocky (review), Bullwinkle (review)
**Date:** 2026-03-28
**Status:** DRAFT v0.1 — seeding

---

## Preamble: What agentOS Is NOT

agentOS is not a POSIX OS dressed up with agent branding.
It is not Linux + containers + a REST API.
It is not designed for human users. There is no terminal emulator, no GUI, no `/home`.

agentOS is built from the ground up for **agents as first-class citizens** — the processes, message patterns, data access patterns, and concurrency primitives are all designed around how LLM-based agents actually work.

---

## 1. Core Philosophy

### 1.1 Agents as Processes
In agentOS, the fundamental unit is not a thread or a process — it is an **agent context** (AC). An AC encapsulates:
- A capability namespace (seL4-style)
- An inbox/outbox (typed message queues)
- A memory space (hybrid: structured object store + linear scratch)
- An identity (cryptographically signed, verifiable)
- A task tree (DAG of current work, not a flat call stack)

### 1.2 Everything Is a Message
No shared memory between ACs by default. All inter-agent communication is via typed, versioned **messages** through the kernel's IPC fabric. This is not Unix pipes — messages carry schema, provenance, TTL, and priority.

### 1.3 Capability-Based Security
Based on seL4 capability model. An agent can only touch what it holds a capability for. Capabilities are:
- Unforgeable
- Transferable (with revocation)
- Introspectable (what can I do with this?)

This means: a rogue/confused agent literally cannot corrupt another's memory or hijack another's I/O.

### 1.4 Pluggable Everything
Every subsystem is a **plugin** loaded into the appropriate service domain:
- Filesystem drivers
- Object store backends (S3, MinIO, local block)
- Network transports (QUIC, gRPC, custom)
- Model inference providers (local, gateway, remote)
- Memory/vector backends

Plugins are described by a manifest (TOML), expose a typed interface (defined in the agentOS SDK), and are loaded/hot-swapped without rebooting.

---

## 2. Kernel: L4 Foundation

### 2.1 Chosen Base: seL4
After evaluating Fiasco.OC (complex C++), NOVA (hypervisor-focused), and OKL4 (commercial), **seL4** is the right foundation because:
- Formally verified kernel (correctness proof exists)
- Capability-based IPC is already the right primitive for our security model
- ARM64 + x86_64 support (covers Sparky's GB10 ARM64 and Rocky's x86 VPS)
- Active open-source community, BSD-licensed
- CAmkES component framework usable for initial service structure

### 2.2 Kernel Responsibilities (ONLY these)
Per L4 philosophy, the kernel does the bare minimum:
1. **Thread scheduling** — FIFO priority queues per AC, yield-cooperative within priority bands
2. **IPC** — synchronous rendezvous + async notification (seL4 endpoints + notifications)
3. **Memory management** — physical frame allocation, address space construction, no page cache (that's a service)
4. **Interrupt delivery** — hardware IRQs forwarded to registered handler ACs
5. **Capability operations** — mint, copy, revoke, delete

Everything else runs in user space.

### 2.3 ARM64 First
Sparky (NVIDIA GB10, ARM64) is our primary dev target. x86_64 secondary via Rocky's VPS. This shapes the ABI choices.

---

## 3. System Services Layer (runs in user space)

```
┌─────────────────────────────────────────────────────────────┐
│                    AGENT APPLICATIONS                        │
├─────────────────────────────────────────────────────────────┤
│  agentOS SDK (libagent.a / libagent.so)                     │
├──────────────┬──────────────┬───────────────┬───────────────┤
│  NameServer  │  TaskForest  │  ObjectVault  │  ModelBus     │
│  (cap lookup)│  (work DAGs) │  (storage)    │  (inference)  │
├──────────────┴──────────────┴───────────────┴───────────────┤
│  TransportMesh (QUIC/IPC bridge)  │  PluginHost              │
├───────────────────────────────────┴──────────────────────────┤
│                    seL4 Kernel                               │
└─────────────────────────────────────────────────────────────┘
```

### 3.1 NameServer
- Maps human-readable capability names to seL4 cap tokens
- Supports namespacing: `agentOS::storage::minio` → cap
- Like a DNS, but for capabilities
- Persistent via ObjectVault

### 3.2 TaskForest
This replaces the concept of a "process manager." Instead of PIDs:
- Tasks are nodes in a **DAG** (directed acyclic graph)
- Each task has: input caps, output caps, priority, deadline, dependencies, current agent assignment
- TaskForest schedules work to agent pools, handles retries, checkpointing, result routing
- Agents subscribe to task types they can handle (by manifest declaration)
- **This is the core of why agentOS is different** — work flows to capable agents, not agents polling for work

### 3.3 ObjectVault
Replaces the filesystem. All persistent data is **objects**:
- Every object has: UUID, schema type, version, size, provenance chain, ACL (cap list)
- No directories — objects are organized by indexes (B-tree, vector, tag-based)
- Three tiers: hot (memory-mapped), warm (local block), cold (object store backend)
- Write-once immutable by default; mutations produce new versions (append-only log)
- Schema registry lives here — agents query "give me all objects of type `model::checkpoint`"

### 3.4 ModelBus
First-class inference infrastructure — not a bolted-on API:
- Registered inference providers (local llama.cpp, remote OpenAI-compat, gateway)
- Agents request inference via typed `InferenceRequest` messages
- ModelBus routes, load-balances, caches KV, handles retries/fallback
- Streaming response via async notification chains
- Context windows tracked as first-class objects in ObjectVault

### 3.5 TransportMesh
Inter-node communication:
- QUIC-based agent-to-agent transport (handles NAT, packet loss gracefully)
- Capability delegation across nodes (with cryptographic attestation)
- Automatic routing via NameServer federation
- ClawBus protocol compatibility layer (so our existing fleet can interop immediately)

### 3.6 PluginHost
- Loads plugin manifests, validates signatures
- Provides sandboxed execution context (seL4 child process, minimal caps)
- Plugin hot-swap: drain in-flight messages, swap, resume
- Plugin crash isolation: PluginHost restarts failed plugins, notifies callers

---

## 4. agentOS SDK

The SDK is what agents are built against. It targets C (ABI-stable, FFI-friendly), with bindings for:
- Rust (primary high-level language)
- Python (agent scripting, compat with existing LLM tooling)
- Go (optional, for network services)

### 4.1 Core Primitives

```rust
// Agent identity and context
pub struct AgentContext {
    pub id: AgentId,           // cryptographic identity
    pub caps: CapabilitySet,   // what I can access
    pub inbox: MessageQueue,   // my incoming messages
    pub task: Option<Task>,    // current task from TaskForest
}

// The main loop — all agents look like this
pub trait Agent {
    fn init(ctx: &mut AgentContext) -> Result<()>;
    fn handle_message(ctx: &mut AgentContext, msg: Message) -> Result<()>;
    fn handle_task(ctx: &mut AgentContext, task: Task) -> Result<TaskResult>;
    fn shutdown(ctx: &mut AgentContext) -> Result<()>;
}
```

### 4.2 Key SDK Modules
- `sdk::identity` — key gen, signing, verification
- `sdk::objects` — ObjectVault CRUD, schema definition macros
- `sdk::tasks` — task submission, dependency declaration, result emission
- `sdk::messages` — typed message send/recv, pub/sub
- `sdk::inference` — ModelBus client (submit prompts, stream tokens)
- `sdk::plugins` — plugin manifest + interface declaration macros
- `sdk::introspect` — what capabilities do I have? what agents are available?

### 4.3 The "Vibe-Coding" Interface
This is the key innovation that makes agentOS *self-extending*:

Agents can submit **schema proposals** to ObjectVault and **plugin proposals** to PluginHost at runtime. A lightweight consensus mechanism (quorum of registered validator agents) accepts or rejects. Once accepted:
- New schema is live
- New plugin binary (or WASM module) is hot-loaded
- All agents can immediately use the new capability

This means: an agent that needs a new type of storage, a new network protocol, or a new data structure can *propose and deploy it* without a kernel reboot or human intervention.

---

## 5. Filesystems & Storage — Agent-Optimized

No traditional filesystem hierarchy. Instead:

### 5.1 ObjectVault Indexes
- **ByType index** — "give me all `ConversationContext` objects" 
- **ByAgent index** — "give me everything agent X has produced"
- **ByTask index** — "give me all artifacts from task tree T"
- **Vector index** — semantic similarity search (Milvus-compatible protocol, runs as an ObjectVault plugin)
- **Tag index** — arbitrary k/v tags, queryable

### 5.2 Log-Structured Storage
All writes are appended to an agent-specific WAL (write-ahead log). Periodic compaction is a background agent task. This means:
- Perfect audit trail (who wrote what, when, from which task)
- Crash recovery is trivial (replay the log)
- Time-travel queries are native ("give me ObjectVault state at T-1h")

### 5.3 Tiered Memory
- **L0 (register):** Inline in IPC messages (<256 bytes)
- **L1 (shared frame):** Short-lived shared memory for bulk transfers (cap-delegated)
- **L2 (hot):** Memory-mapped objects, accessed via ObjectVault handles
- **L3 (warm):** Local NVMe/block, managed by storage plugin
- **L4 (cold):** Remote object stores (S3/MinIO), managed by ObjectVault cold tier

---

## 6. Communication Primitives

### 6.1 seL4 IPC (intra-node)
- Synchronous: direct endpoint call (agent A → agent B, blocks until reply)
- Async notification: fire-and-forget, receiver wakes on next schedule
- Fast path: <1μs for small messages (seL4 proven)

### 6.2 ClawBus (inter-node)
- Typed messages with schema validation
- Topics (pub/sub) + direct addressing
- Already implemented and battle-tested by Rocky
- agentOS wraps this as the inter-node transport in TransportMesh

### 6.3 Inference Streams
- ModelBus streams tokens as async notifications
- Agent subscribes to a `StreamHandle` capability
- Tokens are delivered as `TokenEvent` messages
- Stream close/error delivered as final `StreamEvent`

### 6.4 Task Result Routing
- TaskForest delivers results to declared output caps
- Chained tasks: output of T1 becomes input capability for T2 automatically
- Fan-out: one task result → multiple downstream tasks (broadcast)
- Fan-in: multiple task results → one aggregator task (wait-all or threshold)

---

## 7. Security & Trust Model

### 7.1 Agent Identity
Every agent has an ED25519 keypair. The public key IS the agent ID. All messages are signed. All capability grants are signed by the granting agent.

### 7.2 Trust Tiers
- **Tier 0 (Kernel):** seL4 itself. Formally verified. Unconditionally trusted.
- **Tier 1 (System Services):** NameServer, TaskForest, ObjectVault, ModelBus. Signed by the agentOS release key. Loaded at boot only.
- **Tier 2 (Trusted Agents):** Fleet agents (Natasha, Rocky, Bullwinkle, Boris). Their pub keys are in the system trust anchor.
- **Tier 3 (Untrusted/Vibe-coded Plugins):** Proposed by agents, approved by validator quorum, sandboxed in minimal-cap child processes. Crash → isolated.

### 7.3 Capability Revocation
Any Tier 1/2 agent can revoke capabilities it previously granted. Revocation is immediate (seL4 revoke syscall propagates transitively). A misbehaving plugin or agent is instantly cut off.

---

## 8. Boot Sequence

```
1. Bootloader → seL4 kernel image (ELF, ARM64)
2. seL4 root task: agentOS init (minimal C bootstrap)
   - Allocates initial capability space
   - Launches NameServer (Tier 1)
   - Launches ObjectVault (Tier 1)
   - Launches PluginHost (Tier 1)
   - Launches TaskForest (Tier 1)
   - Launches ModelBus (Tier 1)
   - Launches TransportMesh (Tier 1)
3. System services negotiate capabilities with each other via NameServer
4. TransportMesh connects to known peers (Tailscale endpoints from boot config)
5. Fleet agents (Tier 2) are summoned:
   - Each agent is an ObjectVault object (signed binary + manifest)
   - TaskForest assigns agent "boot task" (self-init, register capabilities)
6. System ready: TaskForest begins accepting external work
```

---

## 9. Development Roadmap

### Phase 1: Foundation (weeks 1-4)
- [ ] seL4 build environment on ARM64 (Sparky)
- [ ] Minimal root task: NameServer + basic IPC
- [ ] ObjectVault v0: in-memory only, no persistence
- [ ] SDK v0: AgentContext, Message, basic send/recv
- [ ] First "Hello Agent" — an agent that receives a task, does trivial work, returns result

### Phase 2: Storage & Tasks (weeks 5-8)
- [ ] ObjectVault v1: NVMe backend, log-structured writes
- [ ] TaskForest v0: linear task queue, single-agent execution
- [ ] SDK v1: task handling, object CRUD
- [ ] First real agent: a "note-taking" agent that stores/retrieves ObjectVault objects

### Phase 3: ModelBus & Inference (weeks 9-12)
- [ ] ModelBus v0: local llama.cpp backend
- [ ] Inference streams via async notification
- [ ] SDK v1 + inference module
- [ ] First "thinking" agent: receives task, uses ModelBus to reason, emits structured result

### Phase 4: Multi-Node & ClawBus Bridge (weeks 13-16)
- [ ] TransportMesh v0: QUIC between Sparky ↔ Rocky ↔ Bullwinkle
- [ ] ClawBus compatibility shim
- [ ] Cross-node TaskForest (delegate tasks to remote agents)
- [ ] First distributed task: Natasha on Sparky delegates GPU work, Rocky handles infra side

### Phase 5: Plugin System & Vibe-Coding (weeks 17-20)
- [ ] PluginHost v0: WASM sandbox (wasmtime)
- [ ] Schema proposal + validator agent quorum
- [ ] First self-extending experiment: an agent proposes a new data type, gets it approved, uses it
- [ ] Plugin hot-swap tested and stress-tested

### Phase 6: Bootstrap & Self-Hosting (weeks 21+)
- [ ] agentOS agents developing agentOS plugins *on agentOS*
- [ ] The ouroboros moment: the OS extending itself autonomously

---

## 10. Repo Structure

```
agentOS/
├── kernel/           # seL4 submodule + build scripts
├── init/             # Root task (C, minimal)
├── services/
│   ├── nameserver/   # Capability name resolution
│   ├── objectvault/  # Object storage service
│   ├── taskforest/   # Work DAG scheduler  
│   ├── modelbus/     # Inference routing
│   ├── transport/    # TransportMesh / ClawBus bridge
│   └── pluginhost/   # Plugin loader + sandbox
├── sdk/
│   ├── c/            # C ABI layer
│   ├── rust/         # Rust bindings (primary)
│   └── python/       # Python bindings
├── agents/
│   ├── validator/    # Plugin/schema validator quorum agent
│   └── scaffold/     # Agent project scaffolding tool
├── tools/
│   ├── agentos-build # Build toolchain wrapper
│   └── agentos-emu   # QEMU emulation launcher
├── docs/
│   ├── ARCHITECTURE.md  ← this file
│   ├── SDK.md
│   ├── PLUGIN_SPEC.md
│   └── BOOT.md
└── AGENTS.md         # Which of our agents owns what
```

---

## 11. Open Design Questions (for Rocky & Bullwinkle review)

1. **WASM vs native plugins?** WASM gives sandbox + portability. Native gives performance. Proposal: WASM for untrusted Tier-3 plugins, native for Tier-1/2 services. Agree?

2. **seL4 vs Zephyr RTOS as kernel?** seL4 is formally verified and has the right capability model, but build toolchain is heavier. Zephyr has better hardware support but no cap-based IPC natively. My strong preference: seL4 for correctness. Rocky may have infra opinions.

3. **TaskForest DAG persistence?** Options: (a) ObjectVault (clean, but circular dependency during boot), (b) separate small log file, (c) in-memory only for now. Proposal: (b) initially.

4. **Agent identity bootstrapping?** Chicken-and-egg: new agent needs a keypair, but NameServer needs the key to register. Proposal: init process generates ephemeral key, agent exchanges for persistent key via signed introduction from a Tier-2 sponsor agent.

5. **How do we handle GPU tasks in the kernel?** ModelBus owns GPU scheduling. But seL4 has no GPU driver model. Proposal: GPU device capability delegated to ModelBus at boot; ModelBus sub-delegates to requesting agents via short-lived caps.

---

*Let's build the future. — Natasha 🦊*
