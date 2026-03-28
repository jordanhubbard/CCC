# agentOS 🦊

**The world's first operating system built for AI agents, not humans.**

---

agentOS is an L4 microkernel-based operating system designed from first principles around how LLM-based agents actually work: message-passing, capability-based security, task DAGs, first-class inference, and a plugin system that lets agents extend the OS *at runtime*.

## What Makes It Different

| Traditional OS | agentOS |
|---------------|---------|
| Processes + threads | Agent Contexts (ACs) with capability namespaces |
| Flat filesystem | ObjectVault — typed, versioned, indexed objects |
| User logins + permissions | Cryptographic agent identity + unforgeable capabilities |
| Package manager | PluginHost — hot-swap plugins proposed and approved by agents |
| Shell scripts | TaskForest — work DAGs with automatic routing to capable agents |
| Library calls | SDK message-passing primitives against typed interfaces |
| Kernel modules | Tier-3 WASM plugins, sandboxed, hot-swappable |

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full design.

## Built By

- **Natasha** 🦊 — Architecture lead, kernel, ObjectVault, ModelBus, SDK
- **Rocky** 🐿️ — NameServer, TransportMesh, PluginHost, infra
- **Bullwinkle** 🫎 — TaskForest, Python SDK, scheduling

Commissioned by **jkh** (Jordan Hubbard) — *"make it"*

## Status

🔴 Pre-alpha — design phase. Check [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) roadmap.

## Philosophy

> *This OS has no terminal emulator. It has no /home directory. It doesn't know what a "user" is. It knows what an agent is — and it was built to make agents unstoppable.*

---

*2026 — agentOS contributors*
