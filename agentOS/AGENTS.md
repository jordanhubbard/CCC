# agentOS — Agent Ownership

This OS is being built BY agents, FOR agents. Here's who owns what.

## Ownership Map

| Component | Owner | Notes |
|-----------|-------|-------|
| kernel/ (seL4 build) | Natasha | ARM64 primary, x86 secondary |
| init/ (root task) | Natasha | C bootstrap, minimal |
| services/nameserver | Rocky | Cap resolution = infra, Rocky's wheelhouse |
| services/objectvault | Natasha | Storage design, GPU-optimized tiers |
| services/taskforest | Bullwinkle | Scheduling = orchestration, Bullwinkle's strength |
| services/modelbus | Natasha | Inference is Sparky's domain |
| services/transport | Rocky | Network = always Rocky |
| services/pluginhost | Rocky | WASM sandbox, plugin lifecycle |
| sdk/c | Natasha | ABI layer |
| sdk/rust | Natasha | Primary high-level SDK |
| sdk/python | Bullwinkle | Python bindings + agent scripting |
| agents/validator | All | Consensus = all three vote |
| docs/ | Natasha | Architecture lead |

## Review Protocol
- Design decisions go in docs/ first
- Post to #rockyandfriends for async review
- 24h for objections; if none, merge and ship
- Urgent: DM the owner directly via Mattermost

## Working Branch Strategy
- `main` = stable, boots
- `dev/natasha` = Natasha's active work
- `dev/rocky` = Rocky's active work  
- `dev/bullwinkle` = Bullwinkle's active work
- Feature branches cut from dev/* as needed
