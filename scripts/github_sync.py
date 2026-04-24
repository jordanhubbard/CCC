#!/usr/bin/env python3
"""
github-sync.py — Two-way GitHub ↔ beads ↔ fleet task sync.

Polls GitHub issues for configured repos and:
  1. Creates/updates beads issues for new/changed GH issues
  2. Creates fleet tasks for issues labelled GITHUB_DISPATCH_LABEL (default: agent-ready)
  3. Closes beads issues when the corresponding GH issue is closed

Runs as a cron/Hermes task or standalone daemon.

Env vars:
  GITHUB_REPOS            Comma-separated owner/repo list  (required)
  GITHUB_DISPATCH_LABEL   Label that triggers fleet task    (default: agent-ready)
  GITHUB_SYNC_INTERVAL    Seconds between polls             (default: 300)
  GITHUB_AUTO_CLOSE       Close GH issue on task complete   (default: false)
  GITHUB_TOKEN            PAT — falls back to gh CLI auth
  ACC_URL / CCC_URL       Hub URL
  ACC_AGENT_TOKEN / CCC_AGENT_TOKEN
  ACC_DATA_DIR            State file directory              (default: ~/.acc/data)
  DRY_RUN                 "true" = no writes
"""

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

# ── Config ────────────────────────────────────────────────────────────────

def _load_env(path):
    if not os.path.exists(path):
        return
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            if k not in os.environ:
                os.environ[k] = v

for _p in ("~/.acc/.env", "~/.ccc/.env", "~/.hermes/.env"):
    _load_env(os.path.expanduser(_p))

ACC_URL        = os.environ.get("ACC_URL") or os.environ.get("CCC_URL", "http://localhost:8789")
ACC_TOKEN      = os.environ.get("ACC_AGENT_TOKEN") or os.environ.get("CCC_AGENT_TOKEN", "")
GITHUB_REPOS   = [r.strip() for r in os.environ.get("GITHUB_REPOS", "").split(",") if r.strip()]
DISPATCH_LABEL = os.environ.get("GITHUB_DISPATCH_LABEL", "agent-ready")
SYNC_INTERVAL  = int(os.environ.get("GITHUB_SYNC_INTERVAL", "300"))
AUTO_CLOSE     = os.environ.get("GITHUB_AUTO_CLOSE", "false").lower() == "true"
DRY_RUN        = os.environ.get("DRY_RUN", "false").lower() == "true"
DATA_DIR       = os.path.expanduser(
    os.environ.get("ACC_DATA_DIR", os.environ.get("CCC_DATA_DIR", "~/.acc/data"))
)
STATE_PATH     = os.path.join(DATA_DIR, "github-sync-state.json")

# ── State persistence (F6) ────────────────────────────────────────────────

def load_state(path: str = STATE_PATH) -> dict:
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return {}

def save_state(path: str, state: dict) -> None:
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    tmp = path + f".tmp.{os.getpid()}"
    with open(tmp, "w") as f:
        json.dump(state, f, indent=2)
        f.write("\n")
    os.replace(tmp, path)

# ── GitHub helpers ────────────────────────────────────────────────────────

def gh_issue_list(repo: str, since: str | None = None) -> list[dict]:
    """Fetch issues via gh CLI. Returns list with .repo injected."""
    cmd = [
        "gh", "issue", "list",
        "--repo", repo,
        "--state", "all",
        "--limit", "200",
        "--json", "number,title,body,labels,state,url,author,createdAt,updatedAt",
    ]
    if since:
        # gh doesn't have --since but we filter client-side; keep for future
        pass
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
        if result.returncode != 0:
            print(f"WARN gh issue list failed for {repo}: {result.stderr.strip()}", file=sys.stderr)
            return []
        issues = json.loads(result.stdout)
        for issue in issues:
            issue["repo"] = repo
        return issues
    except Exception as e:
        print(f"WARN gh CLI error for {repo}: {e}", file=sys.stderr)
        return []

def gh_issue_comment(repo: str, number: int, body: str) -> bool:
    if DRY_RUN:
        print(f"  [dry-run] would comment on {repo}#{number}: {body[:60]}")
        return True
    cmd = ["gh", "issue", "comment", str(number), "--repo", repo, "--body", body]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    return result.returncode == 0

def gh_issue_close(repo: str, number: int) -> bool:
    if DRY_RUN:
        print(f"  [dry-run] would close {repo}#{number}")
        return True
    cmd = ["gh", "issue", "close", str(number), "--repo", repo]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    return result.returncode == 0

# ── beads helpers ─────────────────────────────────────────────────────────

def _bd_bin() -> str:
    import shutil
    return shutil.which("bd") or os.path.expanduser("~/.local/bin/bd")

def bd(*args) -> tuple[int, str]:
    """Run a bd command, return (returncode, stdout)."""
    cmd = [_bd_bin()] + list(args)
    if DRY_RUN and args and args[0] in ("create", "update", "close"):
        print(f"  [dry-run] bd {' '.join(args)}")
        return 0, ""
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    return result.returncode, result.stdout.strip()

def list_beads_issues() -> list[dict]:
    """Return all beads issues as parsed dicts via bd export to stdout."""
    rc, out = bd("export")
    issues = []
    for line in out.splitlines():
        line = line.strip()
        if line.startswith("{"):
            try:
                issues.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return issues

# ── ACC fleet API helpers ─────────────────────────────────────────────────

def _acc_headers() -> dict:
    h = {"Content-Type": "application/json"}
    if ACC_TOKEN:
        h["Authorization"] = f"Bearer {ACC_TOKEN}"
    return h

def acc_get(path: str) -> dict | None:
    req = urllib.request.Request(f"{ACC_URL}{path}", headers=_acc_headers())
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read())
    except Exception as e:
        print(f"WARN ACC GET {path}: {e}", file=sys.stderr)
        return None

def acc_post(path: str, data: dict) -> dict | None:
    if DRY_RUN:
        print(f"  [dry-run] POST {path}: {json.dumps(data)[:120]}")
        return {"ok": True, "task": {"id": "dry-run-task"}}
    body = json.dumps(data).encode()
    req = urllib.request.Request(
        f"{ACC_URL}{path}", data=body, headers=_acc_headers(), method="POST"
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return json.loads(resp.read())
    except Exception as e:
        print(f"WARN ACC POST {path}: {e}", file=sys.stderr)
        return None

def find_project_for_repo(repo: str) -> str | None:
    """Try to find a fleet project matching the repo name."""
    resp = acc_get("/api/projects")
    if not resp:
        return None
    projects = resp if isinstance(resp, list) else resp.get("projects", [])
    repo_name = repo.split("/")[-1].lower()
    for p in projects:
        if p.get("name", "").lower() == repo_name:
            return p.get("id")
    # Fall back to the ACC project if nothing matches
    for p in projects:
        if p.get("name", "").lower() == "acc":
            return p.get("id")
    return projects[0].get("id") if projects else None

def _append_fleet_task_to_notes(beads_id: str, task_id: str) -> None:
    rc, out = bd("export")
    for line in out.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            b = json.loads(line)
        except json.JSONDecodeError:
            continue
        if b.get("id") == beads_id:
            existing_notes = b.get("notes") or ""
            new_notes = f"{existing_notes} fleet_task_id={task_id}".strip()
            bd("update", beads_id, f"--notes={new_notes}")
            return

# ── Core sync logic (F1/F6/F7) ───────────────────────────────────────────

def beads_id_for(issue: dict) -> str:
    return f"gh-{issue['repo']}-{issue['number']}"

def _gh_key(num: int, repo: str) -> str:
    """Canonical dedup key embedded in notes and title."""
    return f"[gh:{repo}#{num}]"

def _parse_notes_meta(b: dict) -> dict:
    """Extract github_number/github_repo from notes or metadata fields."""
    # Check structured metadata first
    meta = b.get("metadata") or {}
    if not isinstance(meta, dict):
        try:
            meta = json.loads(meta) if isinstance(meta, str) else {}
        except Exception:
            meta = {}
    if meta.get("github_number") and meta.get("github_repo"):
        return meta
    # Fall back to notes key=value pairs
    notes = b.get("notes") or ""
    result = {}
    for token in notes.split():
        if "=" in token:
            k, v = token.split("=", 1)
            if k == "github_number":
                try:
                    result["github_number"] = int(v)
                except ValueError:
                    pass
            elif k == "github_repo":
                result["github_repo"] = v
    # Fall back: parse [gh:repo#number] from title
    title = b.get("title", "")
    import re
    m = re.search(r'\[gh:([^#\]]+)#(\d+)\]', title)
    if m:
        result.setdefault("github_repo", m.group(1))
        result.setdefault("github_number", int(m.group(2)))
    return result

def is_already_synced(issue: dict, existing: list[dict]) -> bool:
    num = issue["number"]
    repo = issue["repo"]
    for b in existing:
        meta = _parse_notes_meta(b)
        if meta.get("github_number") == num and meta.get("github_repo") == repo:
            return True
    return False

def find_synced(issue: dict, existing: list[dict]) -> dict | None:
    num = issue["number"]
    repo = issue["repo"]
    for b in existing:
        meta = _parse_notes_meta(b)
        if meta.get("github_number") == num and meta.get("github_repo") == repo:
            return b
    return None

def build_metadata(issue: dict) -> dict:
    return {
        "source": "github",
        "github_number": issue["number"],
        "github_repo": issue["repo"],
        "github_url": issue.get("url", ""),
        "github_labels": [lb["name"] for lb in issue.get("labels", [])],
    }

def has_dispatch_label(issue: dict, label: str = DISPATCH_LABEL) -> bool:
    return any(lb["name"] == label for lb in issue.get("labels", []))

def map_priority(label_names: list[str]) -> int:
    for name in label_names:
        if name in ("P0", "critical"):
            return 0
        if name in ("P1", "bug", "high"):
            return 1
        if name in ("P2", "enhancement", "medium"):
            return 2
        if name in ("P3", "low"):
            return 3
        if name in ("P4", "backlog"):
            return 4
    return 2  # default

def build_fleet_task_payload(issue: dict, beads_id: str, project_id: str) -> dict:
    labels = [lb["name"] for lb in issue.get("labels", [])]
    desc = issue.get("body") or ""
    gh_ref = f"\n\nGitHub: {issue.get('url', '')}  |  Beads: {beads_id}"
    return {
        "title": f"{issue['title']} (#{issue['number']}, {beads_id})",
        "description": desc + gh_ref,
        "project_id": project_id,
        "task_type": "work",
        "phase": "build",
        "priority": map_priority(labels),
        "metadata": {
            "source": "github",
            "github_number": issue["number"],
            "github_repo": issue["repo"],
            "github_url": issue.get("url", ""),
            "beads_id": beads_id,
        },
    }

# ── Sync one repo ─────────────────────────────────────────────────────────

def sync_repo(repo: str, state: dict, existing_beads: list[dict]) -> tuple[int, int, int]:
    """Returns (created, updated, fleet_tasks_created)."""
    since = state.get(repo)
    issues = gh_issue_list(repo, since)
    if not issues:
        return 0, 0, 0

    project_id = find_project_for_repo(repo)
    created = updated = fleet_created = 0
    newest_ts = since or ""

    for issue in issues:
        ts = issue.get("updatedAt") or issue.get("createdAt") or ""
        if ts > newest_ts:
            newest_ts = ts

        meta = build_metadata(issue)
        labels = [lb["name"] for lb in issue.get("labels", [])]
        priority = map_priority(labels)
        existing = find_synced(issue, existing_beads)

        if issue.get("state") == "CLOSED":
            if existing and existing.get("status") == "open":
                print(f"  closing beads {existing['id']} (GH #{issue['number']} closed)")
                bd("close", existing["id"], "--reason=closed on GitHub")
                updated += 1
            continue

        meta_json = json.dumps(meta)
        if not existing:
            print(f"  creating beads issue for {repo}#{issue['number']}: {issue['title'][:60]}")
            gh_key = _gh_key(issue["number"], repo)
            rc, out = bd(
                "create",
                f"--title={issue['title']} {gh_key}",
                f"--description={issue.get('body') or ''}",
                "--type=feature",
                f"--priority={priority}",
                f"--notes=source=github github_number={issue['number']} github_repo={repo} github_url={issue.get('url','')}",
            )
            if rc == 0:
                created += 1
                # Extract new beads ID from output like "✓ Created issue: CCC-xyz — ...".
                # Accept any prefix (ACC-, CCC-, etc.) so the script works across
                # projects — the prefix is chosen by the beads project config.
                import re as _re
                beads_id = None
                m = _re.search(r"\b([A-Z]{2,6}-[a-z0-9]+)\b", out)
                if m:
                    beads_id = m.group(1)
                # Update metadata with github linkage
                if beads_id:
                    notes = (f"source=github github_number={issue['number']} "
                             f"github_repo={repo} github_url={issue.get('url','')}")
                    # F2: if agent-ready label, create fleet task
                    if has_dispatch_label(issue) and project_id:
                        payload = build_fleet_task_payload(issue, beads_id, project_id)
                        resp = acc_post("/api/tasks", payload)
                        if resp and resp.get("ok"):
                            task_id = resp.get("task", {}).get("id", "")
                            notes += f" fleet_task_id={task_id}"
                            print(f"    → fleet task {task_id}")
                            fleet_created += 1
                    bd("update", beads_id, f"--notes={notes}")
        else:
            # Issue exists — check if title/state changed
            if existing.get("title") != issue["title"]:
                print(f"  updating beads {existing['id']}: title changed")
                bd("update", existing["id"], f"--title={issue['title']}")
                updated += 1
            # Re-dispatch if now has agent-ready label but no fleet task yet
            # Only for open (non-closed) beads issues
            existing_status = existing.get("status", "open")
            existing_notes_meta = _parse_notes_meta(existing)
            if (existing_status not in ("closed", "cancelled")
                    and has_dispatch_label(issue)
                    and not existing_notes_meta.get("fleet_task_id")
                    and project_id):
                payload = build_fleet_task_payload(issue, existing["id"], project_id)
                resp = acc_post("/api/tasks", payload)
                if resp and resp.get("ok"):
                    task_id = resp.get("task", {}).get("id", "")
                    print(f"    → fleet task {task_id} for existing beads {existing['id']}")
                    fleet_created += 1
                    _append_fleet_task_to_notes(existing["id"], task_id)

    if newest_ts:
        state[repo] = newest_ts
    return created, updated, fleet_created

# ── Main ──────────────────────────────────────────────────────────────────

def run_once(repos: list[str] | None = None) -> dict:
    repos = repos or GITHUB_REPOS
    if not repos:
        print("WARN: no repos configured — set GITHUB_REPOS=owner/repo,...", file=sys.stderr)
        return {}

    state = load_state(STATE_PATH)
    existing_beads = list_beads_issues()
    results = {}

    for repo in repos:
        print(f"Syncing {repo}…")
        c, u, f = sync_repo(repo, state, existing_beads)
        results[repo] = {"created": c, "updated": u, "fleet_tasks": f}
        print(f"  {repo}: +{c} created, ~{u} updated, {f} fleet tasks")

    if not DRY_RUN:
        save_state(STATE_PATH, state)
        # Export beads JSONL so git backup picks it up
        subprocess.run(
            [_bd_bin(), "export", "--output", os.path.join(
                os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                ".beads", "issues.jsonl"
            )],
            capture_output=True,
        )
    return results

def run_daemon():
    print(f"github-sync daemon starting — interval={SYNC_INTERVAL}s repos={GITHUB_REPOS}")
    while True:
        try:
            run_once()
        except Exception as e:
            print(f"ERROR in sync loop: {e}", file=sys.stderr)
        time.sleep(SYNC_INTERVAL)

if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description="GitHub ↔ beads ↔ fleet sync")
    parser.add_argument("--once", action="store_true", help="Run once and exit")
    parser.add_argument("--daemon", action="store_true", help="Run as daemon")
    parser.add_argument("--dry-run", action="store_true", help="No writes")
    parser.add_argument("repos", nargs="*", help="owner/repo overrides")
    args = parser.parse_args()

    if args.dry_run:
        os.environ["DRY_RUN"] = "true"
        DRY_RUN = True

    if args.daemon:
        run_daemon()
    else:
        result = run_once(args.repos or None)
        print(json.dumps(result, indent=2))
