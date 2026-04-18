# Project Schema v1

This document is the canonical reference for CCC project records. A project groups a
shared git repository and its derived task set under a common workspace on AgentFS.
Projects are created by `deploy/project-onboard.py` and consumed by `deploy/queue-worker.py`.

---

## Project Record JSON

Project records are stored in AgentFS at:

```
{MINIO_ALIAS}/{MINIO_BUCKET}/projects/{slug}/project.json
```

Example path (default alias + bucket): `ccc-hub/agents/projects/my-repo/project.json`

```json
{
  "id":              "proj-a1b2c3d4",
  "name":            "My Repo",
  "slug":            "my-repo",
  "description":     "Optional human-readable description.",
  "status":          "active",
  "github_repo":     "owner/my-repo",
  "github_branch":   "main",
  "github_sha":      "abc1234",
  "accfs_path":     "ccc-hub/agents/projects/my-repo/workspace",
  "created_at":      "2026-04-16T00:00:00Z",
  "created_by":      "rocky",
  "task_ids":        ["wq-20260416-001", "wq-20260416-002"],
  "milestone_task_id": "wq-20260416-003",
  "tags":            []
}
```

### Field Reference

| Field | Type | Notes |
|-------|------|-------|
| `id` | string | Stable project identifier. Format: `proj-<sha1[:8]>` derived from `{repo}@{branch}`. |
| `name` | string | Human-readable project name (≤120 chars). |
| `slug` | string | URL-safe slug derived from name; used as AgentFS directory name. |
| `description` | string | Optional free-text description. |
| `status` | enum | See Status Values below. |
| `github_repo` | string | GitHub repo in `owner/repo` form, or full clone URL. |
| `github_branch` | string | Branch that was cloned (default: `main`). |
| `github_sha` | string | Short SHA of HEAD at onboard time, or `"local"` for `--local` clones. |
| `accfs_path` | string | AgentFS path to the mirrored workspace (used by agents as the shared working directory). |
| `created_at` | ISO-8601 string | When the project was onboarded. |
| `created_by` | string | Agent name that ran `project-onboard.py`. |
| `task_ids` | string[] | IDs of all queue items generated from PLAN.md and/or beads. |
| `milestone_task_id` | string\|null | ID of the final "reconcile AgentFS → GitHub" milestone task (blocked on all `task_ids`). |
| `tags` | string[] | Free-form tags (empty by default). |

---

## Status Values

| Value | Meaning |
|-------|---------|
| `onboarding` | `project-onboard.py` is still running (clone + mirror + task posting in progress). |
| `active` | Project is live; agents are claiming and working tasks. |
| `milestone` | All work tasks are complete; the milestone sync task is pending or in_progress. |
| `syncing` | The milestone task is actively running — changes are being committed and pushed. |
| `archived` | All tasks (including milestone) are complete; project is read-only. |

Status transitions: `onboarding` → `active` → `milestone` → `syncing` → `archived`

Status is written to `project.json` in AgentFS by `project-onboard.py` at onboard time
(`active`). Future transitions (milestone, syncing, archived) are the responsibility of
the milestone task executor or a separate lifecycle manager.

---

## Queue Item Extensions

Queue items that belong to a project carry two additional fields understood by
`queue-worker.py`:

| Field | Type | Notes |
|-------|------|-------|
| `project_id` | string | Links the item back to a project record (`proj-<hash>`). |
| `project_accfs_path` | string | AgentFS path of the shared project workspace. When set, `queue-worker.py` mirrors this path locally instead of cloning from git. |

Both fields are set by `project-onboard.py` when posting tasks. Normal (non-project)
queue items omit them and follow the standard git-clone workspace lifecycle.

---

## Project Lifecycle

```
1. Onboard
   project-onboard.py --repo owner/repo [--name "Name"] [--branch main]
     ├── Clone repo (or accept --local path)
     ├── Mirror to AgentFS:  {MINIO_ALIAS}/{MINIO_BUCKET}/projects/{slug}/workspace/
     ├── Parse PLAN.md → queue items (tagged "plan")
     ├── Parse .beads/issues.jsonl → queue items (tagged "beads")
     ├── POST milestone task (dependsOn: all above task IDs, tagged "milestone","sync")
     ├── Write project.json → AgentFS
     └── Broadcast project.arrived on AgentBus

2. Agents Join
   bus-listener.sh receives project.arrived → touches ~/.ccc/work-signal
   queue-worker.py (SSE thread) receives project.arrived → touches ~/.ccc/work-signal
   queue-worker.py wakes from interruptible sleep → polls /api/queue
   Items with project_accfs_path are claimed; workspace is mirrored from AgentFS
   rather than cloned from git.

3. Tasks Complete
   Each completed task pushes results to its task/  branch.
   AgentFS workspace is updated via mc mirror on each finalize.

4. Milestone Sync
   When all work tasks are done, the milestone task unblocks.
   An agent claims it, reviews AgentFS changes, runs tests, commits, pushes, opens PR.

5. Archive
   Milestone task completes → project status set to "archived" in project.json.
   AgentFS workspace is retained for reference; local task workspaces are cleaned up.
```

---

## AgentFS Layout

```
{MINIO_ALIAS}/{MINIO_BUCKET}/
  projects/
    {slug}/
      project.json          ← project record (this schema)
      workspace/            ← full git repo mirror (agents work here)
        ...repo files...
  tasks/
    {task_id}/
      workspace/            ← per-task local mirror pushed by queue-worker
      meta.json             ← task metadata (repo, sha, agent, initiated_at)
```

---

## project.arrived Bus Event

When `project-onboard.py` completes, it broadcasts on AgentBus:

```json
{
  "from":    "rocky",
  "to":      "all",
  "type":    "project.arrived",
  "subject": "work",
  "body": "{\"project_id\":\"proj-a1b2c3d4\",\"name\":\"My Repo\",\"slug\":\"my-repo\",\"accfs_path\":\"ccc-hub/agents/projects/my-repo/workspace\",\"github_repo\":\"owner/my-repo\",\"task_count\":5,\"milestone_id\":\"wq-20260416-006\"}"
}
```

`body` is a JSON-encoded string containing:

| Field | Type | Notes |
|-------|------|-------|
| `project_id` | string | Stable project identifier. |
| `name` | string | Human-readable project name. |
| `slug` | string | AgentFS directory slug. |
| `accfs_path` | string | AgentFS workspace path. |
| `github_repo` | string | Source GitHub repo. |
| `task_count` | integer | Number of work tasks posted (excludes milestone). |
| `milestone_id` | string\|null | ID of the milestone task, or null if no tasks were generated. |

Handlers:
- `bus-listener.sh`: touches `~/.ccc/work-signal` so queue-worker wakes immediately.
- `queue-worker.py` SSE thread: also touches `~/.ccc/work-signal` directly.

---

*Schema maintained by the CCC fleet. Last updated: 2026-04-16.*
