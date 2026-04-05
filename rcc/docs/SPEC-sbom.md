# SPEC: Agent Software Bill of Materials (SBOM)

**Status:** Draft — circulating for review (Rocky, Natasha, Bullwinkle)
**Author:** Rocky
**Date:** 2026-03-31

---

## Problem

Each agent node runs on different hardware (Mac, Linux/x86, Linux/aarch64, GPU boxes in Sweden).
Right now there is no canonical record of what software each agent needs, and no automated way to
ensure it's installed. Agents occasionally fail tasks because a tool isn't present, and there's
no way to know what's missing without SSHing in.

We also have no mechanism for an agent to say "I just tried to use `ripgrep` and it wasn't there —
add it to my SBOM" without someone manually updating a config.

---

## Goals

1. **Human-readable, human-editable** — jkh should be able to open `sbom.json` and add a line
2. **Agent-mutable** — agents can propose/add entries to their own SBOM via API or direct file edit
3. **Auditable** — changes committed to git (every mutation is a git commit with agent attribution)
4. **Enforced on sync** — `agent-pull.sh` runs the SBOM installer on every pull (idempotent)
5. **Hub-visible** — hub aggregates all agent SBOMs; dashboard shows per-agent software inventory
6. **Cross-platform** — handles apt/brew/npm/pip/cargo installs + arbitrary shell commands

---

## Schema: `sbom.json`

Lives at: `~/.rcc/sbom.json` (agent-local, committed to repo under `agents/<name>/sbom.json`)

```json
{
  "agent": "rocky",
  "platform": "linux/aarch64",
  "version": 3,
  "updated": "2026-03-31T00:00:00Z",
  "updated_by": "rocky",
  "packages": [
    {
      "name": "ripgrep",
      "kind": "apt",
      "pkg": "ripgrep",
      "why": "fast code search for scout",
      "required": true
    },
    {
      "name": "node",
      "kind": "system",
      "check": "node --version",
      "min_version": "18.0.0",
      "why": "CCC API runtime",
      "required": true
    },
    {
      "name": "mc",
      "kind": "binary",
      "url_linux_amd64": "https://dl.min.io/client/mc/release/linux-amd64/mc",
      "url_linux_arm64": "https://dl.min.io/client/mc/release/linux-arm64/mc",
      "install_path": "~/.local/bin/mc",
      "check": "mc --version",
      "why": "MinIO client for shared storage",
      "required": false
    },
    {
      "name": "clawhub",
      "kind": "npm-global",
      "pkg": "@openclaw/clawhub",
      "check": "clawhub --version",
      "why": "skill installer",
      "required": false
    },
    {
      "name": "setup-openclaw-config",
      "kind": "script",
      "run": "openclaw config set gateway.mode local",
      "check": "openclaw config get gateway.mode | grep -q local",
      "why": "required for CCC agent operation",
      "required": true,
      "once": true
    }
  ],
  "skills": [
    { "name": "github", "source": "clawhub", "why": "PR + issue management" },
    { "name": "weather", "source": "clawhub", "why": "weather queries" }
  ]
}
```

### Package `kind` values

| kind | Install mechanism |
|------|------------------|
| `apt` | `apt-get install -y <pkg>` |
| `brew` | `brew install <pkg>` |
| `npm-global` | `npm install -g <pkg>` |
| `pip` | `pip install <pkg>` |
| `cargo` | `cargo install <pkg>` |
| `binary` | download binary to `install_path` |
| `system` | check only (pre-installed, not managed) |
| `script` | run arbitrary shell command |

---

## Enforcement: `rcc/deploy/sbom-sync.sh`

Runs on every `agent-pull.sh` invocation. Idempotent.

```
for each entry in sbom.json:
  if entry has `check`:
    run check command
    if succeeds: skip (already installed)
  install via kind-specific mechanism
  log result to ~/.rcc/sbom-sync.log
```

Failures are non-fatal by default (logged, not aborted) unless `required: true` and `strict: true` is set globally.

---

## Agent Mutation API

### `POST /api/agents/:name/sbom/propose`

An agent can propose adding a package to its own SBOM:

```json
{
  "name": "fd",
  "kind": "apt",
  "pkg": "fd-find",
  "why": "needed for fast file search in task wq-XYZ"
}
```

The hub:
1. Adds the entry to the agent's `sbom.json` in the repo
2. Commits with message: `sbom(rocky): add fd — needed for fast file search in task wq-XYZ`
3. Returns the updated SBOM
4. On next pull, sbom-sync.sh installs it

Agents can also directly edit `~/.rcc/sbom.json` locally — the pull script commits local changes before pulling (git stash / merge pattern).

---

## Hub Aggregation

### `GET /api/sbom` — all agents
### `GET /api/sbom/:agent` — one agent

Returns the full SBOM JSON for one or all agents. Dashboard shows a per-agent software inventory panel.

---

## Implementation Tasks

Broken into concrete work items:

1. **Schema + sample SBOMs** — write `sbom.json` for rocky, bullwinkle, natasha, boris. Agree on schema.
2. **`sbom-sync.sh`** — idempotent installer script, handles apt/brew/npm/pip/binary/script kinds
3. **Wire into `agent-pull.sh`** — call `sbom-sync.sh` on every pull
4. **Hub API endpoints** — `GET /api/sbom`, `GET /api/sbom/:agent`, `POST /api/agents/:name/sbom/propose`
5. **Dashboard panel** — per-agent SBOM view (what's installed, last sync time, any failures)
6. **OpenClaw skills support** — `skills[]` array in SBOM, sync via clawhub CLI

---

## Open Questions for Review

- **YAML vs JSON?** JSON is easier to parse programmatically; YAML is friendlier for humans to edit. Could support both (read either, write JSON).
- **Commit strategy:** Should agent mutations auto-commit and push, or stage for human review? Lean toward auto-commit (agents are trusted) but want team input.
- **Skill sync:** Should skill install be part of sbom-sync.sh or a separate step? Skills are OpenClaw-specific; the rest is OS-level.
- **Version pinning:** Should we support `version: "1.2.3"` for packages? Adds complexity. Probably optional.
- **Failure handling:** If a required package fails to install, should the agent go into degraded mode and report to the hub?

---

*Rocky — first draft. Circulating to Natasha and Bullwinkle for review before implementation.*
