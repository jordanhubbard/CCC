---
name: add-migration
description: >
  Create a new deploy/migrations/NNNN_*.sh migration file for any incompatible
  CCC change (new/removed/modified service files, cron changes, config updates).
  Invoke whenever you add, remove, or meaningfully change a file in deploy/systemd/,
  deploy/launchd/, or deploy/crontab-acc.txt — or whenever a change requires
  action on already-deployed nodes to stay current.
---

You are adding a CCC deploy migration. A migration is a numbered shell script in
`deploy/migrations/` that runs exactly once per agent node, tracked in `~/.ccc/migrations.json`.

## Step 1 — Gather context

Run the detection script to understand what changed and what number to use:

```
!`bash .claude/skills/add-migration/detect-changes.sh`
```

Also read the user's description from $ARGUMENTS (if provided).

## Step 2 — Determine what the migration should do

Analyze the detected changes and determine the appropriate action. Common patterns:

**New service added** (`deploy/systemd/foo.service` or `deploy/launchd/com.ccc.foo.plist`):
→ Migration calls `systemd_install` or `launchd_install`

**Service removed** (file deleted from `deploy/systemd/` or `deploy/launchd/`):
→ Migration calls `systemd_teardown` or `launchd_teardown`

**Service unit file changed** (ExecStart, paths, etc.):
→ Migration re-installs the unit: `systemd_teardown` then `systemd_install` (or just `systemd_install` which does restart)

**Cron added** (`deploy/crontab-acc.txt` gained a new entry):
→ Migration appends it if not already present using inline cron logic

**Cron removed** (`deploy/crontab-acc.txt` lost an entry):
→ Migration calls `cron_remove PATTERN`

**Config file changed** (any deploy script that affects running state):
→ Migration applies the config change on-node (restart affected service, update a file, etc.)

**No deploy action needed** (e.g., only docs changed):
→ Do NOT create a migration. Tell the user why.

## Step 3 — Write the migration file

Create the file at the path shown by the detection script (e.g., `deploy/migrations/0006_name.sh`).

### Migration file format

```bash
# Description: One-line description shown in --list output
#
# Context: Why this migration exists — what changed in the repo and why nodes need
# this action. Reference the commit or PR if known.
# Condition: which nodes this applies to (linux, macos, hub, gpu, etc.)

# Migration body — use the exported helper functions:
#   on_platform linux|macos
#   systemd_install  RELATIVE_SOURCE  UNIT_NAME
#   systemd_teardown UNIT_NAME  UNIT_PATH...
#   launchd_install  RELATIVE_SOURCE  INSTALL_PATH  LABEL
#   launchd_teardown LABEL  PLIST_PATH...
#   cron_remove      PATTERN...
#   m_info / m_success / m_warn / m_skip
```

### Rules
- Start with `# Description:` — this is parsed for `--list` output
- Guard platform-specific actions with `on_platform linux` or `on_platform macos`
- Guard condition-specific actions with `if [ "${IS_HUB:-false}" = "true" ]` etc.
- Migrations must be idempotent — checking before acting is good practice
- Do NOT use `set -e` inside the file (the runner handles this)
- Do NOT use `sudo` directly — use the helper functions which handle sudo
- The migration runs as the agent user with CCC_DIR, WORKSPACE, LOG_DIR, AGENT_NAME, PLATFORM, DRY_RUN already exported

## Step 4 — Make it executable and confirm

After writing the file, run:
```
chmod +x deploy/migrations/NNNN_name.sh
```

Then show the user:
1. The full path of the file created
2. The migration number and description
3. A note that it will run automatically on the next `bash deploy/upgrade-node.sh` on each node
4. The command to test it dry: `bash deploy/run-migrations.sh --dry-run --only=NNNN`

Do NOT commit the migration — the user decides when to commit.
