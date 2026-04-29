---
name: acc-agent-bootstrap
description: Verify and install the native ACC agent runtime on an ACC node. Use when onboarding a new agent, confirming `acc-agent hermes` is operational, or diagnosing why an agent isn't executing tasks.
version: 1.2.0
platforms: [linux, macos]
metadata:
  hermes:
    tags: [acc, hermes, bootstrap, install, agentfs]
    category: infrastructure
---

# ACC Agent Bootstrap

## Why PATH detection is unreliable over SSH

Non-interactive SSH sessions do **not** source `~/.bashrc` or `~/.profile`.
`command -v acc-agent` can return nothing even when the runtime is installed,
because `~/.acc/bin` is only added to PATH in interactive shells.

**Never rely on `which` alone over SSH.** Check the canonical path first:

```bash
ssh -o StrictHostKeyChecking=no USER@HOST \
  'test -x ~/.acc/bin/acc-agent && ~/.acc/bin/acc-agent hermes --query "health check"'
```

---

## Step 1 — Check if acc-agent is already installed

```bash
# Reliable: search known locations
ssh -o StrictHostKeyChecking=no USER@HOST \
  'find ~/.acc/bin /usr/local/bin ~/.cargo/bin -name acc-agent -type f 2>/dev/null'

# Also check the workspace source
ssh -o StrictHostKeyChecking=no USER@HOST \
  'test -d ~/.acc/workspace && cd ~/.acc/workspace && git rev-parse --short HEAD'
```

If found, verify the native Hermes subcommand:

```bash
ssh -o StrictHostKeyChecking=no USER@HOST '~/.acc/bin/acc-agent hermes --query "hello"'
```

For interactive host debugging:

```bash
ssh -t USER@HOST '~/.acc/bin/acc-agent hermes --chat'
```

---

## Step 2 — Install or refresh acc-agent

The native runtime is built from the ACC workspace and installed to
`~/.acc/bin/acc-agent`:

```bash
ssh -o StrictHostKeyChecking=no USER@HOST \
  'cd ~/.acc/workspace && git fetch origin && git reset --hard origin/main && bash deploy/restart-agent.sh'
```

This is also the preferred repair path for stale Slack gateways because it kills
old `hermes gateway` processes and restarts `acc-agent hermes --gateway`.

---

## Step 3 — Verify gateway and worker modes

Gateway:

```bash
ssh -o StrictHostKeyChecking=no USER@HOST \
  'pgrep -af "acc-agent hermes --gateway" || true'
```

Worker:

```bash
ssh -o StrictHostKeyChecking=no USER@HOST \
  'pgrep -af "acc-agent (bus|tasks|hermes --poll|supervise)" || true'
```

---

## Step 4 — Verify ANTHROPIC_BASE_URL

Hermes routes LLM calls through Tokenhub, not the public Anthropic API.
The agent's `.env` must have `ANTHROPIC_BASE_URL` pointing to the local proxy:

```bash
ssh -o StrictHostKeyChecking=no USER@HOST \
  'grep ANTHROPIC_BASE_URL ~/.acc/.env 2>/dev/null || grep ANTHROPIC_BASE_URL ~/.ccc/.env 2>/dev/null || echo "NOT SET"'
```

Expected: `ANTHROPIC_BASE_URL=http://localhost:9099` (or the Tokenhub address).
If missing, add it to `~/.acc/.env`.

---

## Step 5 — Verify acc-agent queue subcommand supports hermes

The `acc-agent queue` worker only runs hermes tasks if the binary was built with
hermes support. Check:

```bash
ssh -o StrictHostKeyChecking=no USER@HOST '~/.acc/bin/acc-agent 2>&1 | grep hermes'
```

Expected output includes `acc-agent hermes`. If the subcommand is missing, the
binary needs to be rebuilt (run `agent-pull.sh` which triggers a Cargo build if
`agent/` source changed).

---

## Step 6 — Verify AgentFS mount

AgentFS is a Samba (CIFS) share served from Rocky (`100.89.199.14`), share name
`accfs`, exporting `/srv/accfs/shared`. Every agent should have it mounted.

**Expected mount point and visible content:**

| Agent | Mount point | Sees |
|---|---|---|
| rocky | `/srv/accfs/shared` (local) | `Sim-Next/` etc. |
| natasha | `~/.acc/shared` (CIFS) | `Sim-Next/` etc. |
| ollama | `~/.acc/shared` (CIFS) | `Sim-Next/` etc. |
| bullwinkle | `~/.acc/shared` (CIFS, macOS) | `Sim-Next/` etc. |
| boris | `~/.acc/shared` (Kubernetes PVC needed) | not yet mounted |

**Check the mount is live (not stale):**

```bash
ssh -o StrictHostKeyChecking=no USER@HOST '
  mount | grep accfs && echo "mounted" || echo "NOT MOUNTED"
  ls ~/.acc/shared/ 2>/dev/null || echo "stale or empty"
'
```

**If the mount is stale** (e.g. after samba server reconfiguration):

Linux (systemd):
```bash
sudo systemctl restart home-jkh-.acc-shared.mount   # system-level — requires sudo
```

macOS (launchd):
```bash
launchctl unload ~/Library/LaunchAgents/com.acc.accfs-mount.plist
diskutil unmount force ~/.acc/shared
launchctl load ~/Library/LaunchAgents/com.acc.accfs-mount.plist
```

**Important macOS note**: `ls ~/.acc/shared/` from an SSH session returns
`Operation not permitted` due to macOS TCC security. This does NOT mean the
mount is broken. Verify via `mount | grep accfs` and `stat ~/.acc/shared/`.
Agent processes launched via launchd have full access.

**If the mount is missing entirely** (new agent): see `acc-service-setup` skill
for the systemd mount unit template, or `acc-deploy-agentfs` for the setup script.

---

## Step 7 — Run a smoke test

```bash
ssh -o StrictHostKeyChecking=no USER@HOST '
  source ~/.acc/.env 2>/dev/null || source ~/.ccc/.env 2>/dev/null
  export PATH="$HOME/.local/bin:$PATH"
  hermes --version && echo "hermes OK"
'
```

---

## Quick reference: per-agent known paths

| Agent | Init | runtime path | AgentFS |
|---|---|---|---|
| rocky | systemd (system) | `~/.acc/bin/acc-agent` | `/srv/accfs/shared` (local) |
| natasha | systemd (user) | `~/.acc/bin/acc-agent` | `~/.acc/shared` (CIFS) |
| bullwinkle | launchd (macOS) | `~/.acc/bin/acc-agent` | `~/.acc/shared` (CIFS) |
| ollama | systemd (system) | `~/.acc/bin/acc-agent` | `~/.acc/shared` (CIFS) |
| boris | supervisord (K8s) | `~/.acc/bin/acc-agent` | `~/.acc/shared` (PVC needed) |

Always verify against the live agent — these can drift.

## Common mistakes

- **`which acc-agent` over SSH returns nothing**: Non-interactive SSH doesn't load `~/.bashrc`. Use `~/.acc/bin/acc-agent` directly.
- **Legacy `hermes gateway` is running**: Run `bash ~/.acc/workspace/deploy/restart-agent.sh` to replace it with `acc-agent hermes --gateway`.
- **`ANTHROPIC_BASE_URL` not set**: Hermes calls will go to the public Anthropic API (wrong billing, wrong routing). Always set this to the local Tokenhub proxy.
- **AgentFS stale after samba reconfiguration**: Unmount and remount. Do NOT just retry `ls` — the mount handle is broken and must be refreshed.
- **AgentFS `ls` fails on macOS over SSH**: macOS TCC blocks SSH from listing CIFS mounts. The mount is live if `mount | grep accfs` shows it. Check with `stat` instead of `ls`.
- **Boris (Kubernetes) can't mount CIFS**: Containers lack `CAP_SYS_ADMIN`. Use the `/api/fs/` HTTP API instead (see below).

---

## AgentFS via HTTP API (containers without CAP_SYS_ADMIN)

For agents that cannot mount CIFS (Kubernetes pods, restricted containers),
the acc-server exposes AgentFS over HTTP. This covers all practical needs:
listing, reading, writing, and deleting files.

All paths are relative to `/srv/accfs` on the hub. `/shared/Sim-Next/...`
is the same content other agents see at `~/.acc/shared/Sim-Next/...`.

```bash
source ~/.acc/.env 2>/dev/null

# List a directory
curl -sf -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  "${ACC_URL}/api/fs/list?path=/shared"

# Read a file
curl -sf -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  "${ACC_URL}/api/fs/read?path=/shared/Sim-Next/CLAUDE.md"

# Write a file
curl -sf -X POST \
  -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d "$(python3 -c "import json,sys; print(json.dumps({'path':sys.argv[1],'content':sys.argv[2]}))" \
       "/shared/Sim-Next/notes.md" "content here")" \
  "${ACC_URL}/api/fs/write"

# Delete a file
curl -sf -X DELETE \
  -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  "${ACC_URL}/api/fs/delete?path=/shared/Sim-Next/notes.md"

# Check existence (HEAD — 200 = exists, 404 = not found)
curl -sf -I -H "Authorization: Bearer $ACC_AGENT_TOKEN" \
  "${ACC_URL}/api/fs/exists?path=/shared/Sim-Next/CLAUDE.md"
```

The limitation: `agentfs_path` values in project records are server-local
paths (`/srv/accfs/shared/<slug>`). To read a project's `.beads/issues.jsonl`
via the API, translate: strip `/srv/accfs` prefix and use the remainder as
the `path` parameter.
