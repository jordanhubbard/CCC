#!/usr/bin/env python3
"""
hermes-driver.py — CCC-aware supervisor for hermes-agent sessions.

Solves the "compact and stall" problem: when hermes exits with completed=False
(budget exhaustion, mid-work stop) this driver resumes the session automatically,
posting heartbeats to the CCC hub throughout and marking queue items complete
when hermes produces a final summary.

Usage:
    python3 hermes-driver.py --item <queue-item-id> --query "task description"
    python3 hermes-driver.py --resume <session-id>   # resume existing session
    python3 hermes-driver.py --poll                  # poll /api/queue for hermes tasks

Environment (loaded from ~/.ccc/.env):
    CCC_URL           Hub base URL
    CCC_AGENT_TOKEN   Agent bearer token
    AGENT_NAME        Agent name (used in heartbeats)
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

# ── Config ────────────────────────────────────────────────────────────────────

CCC_DIR = Path(os.environ.get("HOME", "/home")) / ".ccc"
ENV_FILE = CCC_DIR / ".env"
LOG_FILE = CCC_DIR / "logs" / "hermes-driver.log"

MAX_RESUME_ATTEMPTS = 6       # max times to restart a stalled session
KEEPALIVE_INTERVAL = 120      # seconds between CCC heartbeats while hermes runs
POLL_INTERVAL = 60            # seconds between queue checks in --poll mode
HERMES_MAX_ITERATIONS = 120   # pass to hermes for long-running tasks


LOG_FILE.parent.mkdir(parents=True, exist_ok=True)


def _log(msg: str) -> None:
    ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    line = f"[{ts}] [hermes-driver] {msg}"
    print(line, flush=True)
    with open(LOG_FILE, "a") as f:
        f.write(line + "\n")


# ── Env loading ───────────────────────────────────────────────────────────────

def _load_env() -> None:
    if ENV_FILE.exists():
        with open(ENV_FILE) as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    key, _, val = line.partition("=")
                    os.environ.setdefault(key.strip(), val.strip().strip("'\""))


_load_env()

CCC_URL = os.environ.get("CCC_URL", "").rstrip("/")
CCC_AGENT_TOKEN = os.environ.get("CCC_AGENT_TOKEN", "")
AGENT_NAME = os.environ.get("AGENT_NAME", os.uname().nodename.split(".")[0])


# ── HTTP helpers ──────────────────────────────────────────────────────────────

def _curl(method: str, path: str, body: dict | None = None) -> dict | None:
    if not CCC_URL or not CCC_AGENT_TOKEN:
        return None
    cmd = [
        "curl", "-sf", "--max-time", "10",
        "-X", method,
        "-H", f"Authorization: Bearer {CCC_AGENT_TOKEN}",
        "-H", "Content-Type: application/json",
    ]
    if body is not None:
        cmd += ["-d", json.dumps(body)]
    cmd.append(f"{CCC_URL}{path}")
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=15)
        if r.returncode == 0 and r.stdout.strip():
            return json.loads(r.stdout)
        return None
    except Exception:
        return None


def _heartbeat(note: str = "ok") -> None:
    _curl("POST", f"/api/heartbeat/{AGENT_NAME}", {
        "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "status": "ok",
        "note": note[:200],
    })


def _claim(item_id: str) -> bool:
    resp = _curl("POST", f"/api/item/{item_id}/claim", {
        "agent": AGENT_NAME, "note": "hermes-driver claiming",
    })
    return resp is not None


def _keepalive(item_id: str, note: str) -> None:
    _curl("POST", f"/api/item/{item_id}/keepalive", {"agent": AGENT_NAME, "note": note})


def _complete(item_id: str, result: str) -> None:
    _curl("POST", f"/api/item/{item_id}/complete", {
        "agent": AGENT_NAME, "result": result[:4000], "resolution": result[:4000],
    })


def _fail(item_id: str, reason: str) -> None:
    _curl("POST", f"/api/item/{item_id}/fail", {"agent": AGENT_NAME, "reason": reason[:2000]})


# ── Hermes invocation ─────────────────────────────────────────────────────────

def _find_hermes() -> str:
    """Locate the hermes binary."""
    for candidate in [
        os.path.expanduser("~/.local/bin/hermes"),
        "/usr/local/bin/hermes",
        "/opt/homebrew/bin/hermes",
    ]:
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    # Try PATH
    r = subprocess.run(["which", "hermes"], capture_output=True, text=True)
    if r.returncode == 0:
        return r.stdout.strip()
    raise FileNotFoundError("hermes CLI not found — install hermes-agent first")


def _hermes_uses_subcommands(hermes_bin: str) -> bool:
    """Return True if this hermes build uses 'hermes chat' subcommand style (v0.9+)."""
    r = subprocess.run([hermes_bin, "chat", "--help"], capture_output=True, text=True, timeout=5)
    return r.returncode == 0


def _run_hermes(
    query: str | None = None,
    session_id: str | None = None,
    item_id: str | None = None,
    max_iterations: int = HERMES_MAX_ITERATIONS,
) -> tuple[str, bool, str | None]:
    """
    Run hermes for one session.

    Returns (output_text, completed, new_session_id).
    new_session_id may differ from session_id if compression rotated it.
    """
    hermes = _find_hermes()

    # v0.9+ uses subcommands: hermes chat -q "..." -Q --max-turns N
    # Older builds use top-level flags: hermes --query "..." --quiet --max-iterations N
    if _hermes_uses_subcommands(hermes):
        cmd = [hermes, "chat", "--max-turns", str(max_iterations), "-Q"]
        if session_id:
            cmd += ["--resume", session_id]
        elif query:
            cmd += ["-q", query]
        else:
            raise ValueError("Either query or session_id required")
    else:
        cmd = [hermes, "--max-iterations", str(max_iterations), "--quiet"]
        if session_id:
            cmd += ["--resume", session_id]
        elif query:
            cmd += ["--query", query]
        else:
            raise ValueError("Either query or session_id required")

    env = {**os.environ}
    if item_id:
        env["CCC_QUEUE_ITEM_ID"] = item_id
    # Propagate task workspace vars so Claude Code session knows where to work
    # These are set by queue-worker before calling hermes-driver.
    # If running hermes-driver standalone, callers should set them manually.
    for var in ("TASK_ID", "TASK_WORKSPACE_LOCAL", "TASK_WORKSPACE_AGENTFS", "TASK_BRANCH"):
        if var in os.environ:
            env[var] = os.environ[var]
    # Disable interactive gateway for batch mode
    env.setdefault("HERMES_PLATFORM", "cli")
    env.setdefault("HERMES_QUIET", "1")

    _log(f"Running hermes: {' '.join(cmd[:4])}...")
    try:
        proc = subprocess.run(
            cmd, env=env, capture_output=True, text=True,
            timeout=HERMES_MAX_ITERATIONS * 120,  # generous wall-clock limit
        )
    except subprocess.TimeoutExpired:
        _log("hermes process timed out (wall-clock)")
        return "hermes timed out", False, session_id

    output = proc.stdout.strip() or proc.stderr.strip() or "(no output)"
    completed = proc.returncode == 0
    _log(f"hermes exited {proc.returncode} ({len(output)} chars output)")

    # Try to extract the new session ID from hermes output (it prints it on compression)
    new_session_id = session_id
    for line in (proc.stdout or "").splitlines():
        if "session_id:" in line.lower() or "session:" in line.lower():
            parts = line.split()
            for p in parts:
                if p.startswith("20") and "_" in p:  # looks like a session ID
                    new_session_id = p
                    break

    return output, completed, new_session_id


# ── Main task runner ──────────────────────────────────────────────────────────

def run_task(
    query: str | None,
    item_id: str | None,
    session_id: str | None = None,
) -> None:
    """Run a hermes task, resuming on stall, posting heartbeats and results."""
    _log(f"Starting task: item={item_id} session={session_id} query={str(query)[:60] if query else None}")

    if item_id and not session_id:
        if not _claim(item_id):
            _log(f"Claim rejected for {item_id} — skipping")
            return

    # Prepend workspace instructions to query if workspace is set and this is attempt 1
    workspace_local = os.environ.get("TASK_WORKSPACE_LOCAL", "")
    if query and workspace_local and session_id is None:
        query = (
            f"Your task workspace is: {workspace_local}\n"
            "Work only within this directory. Do NOT run git commit or git push — "
            "the queue-worker will handle the single git push on completion.\n\n"
            + query
        )

    attempt = 0
    current_session = session_id
    final_output = ""
    completed = False

    while attempt < MAX_RESUME_ATTEMPTS:
        attempt += 1
        _log(f"Attempt {attempt}/{MAX_RESUME_ATTEMPTS} session={current_session}")
        _heartbeat(f"hermes attempt {attempt}")

        # Keepalive thread while hermes runs
        stop_ka = [False]
        def _ka_loop(iid, sess, att):
            while not stop_ka[0]:
                time.sleep(KEEPALIVE_INTERVAL)
                if stop_ka[0]:
                    break
                note = f"hermes running (attempt {att}, session={sess})"
                _heartbeat(note)
                if iid:
                    _keepalive(iid, note)
        import threading
        ka_thread = threading.Thread(
            target=_ka_loop, args=(item_id, current_session, attempt), daemon=True
        )
        ka_thread.start()

        try:
            output, completed, new_session = _run_hermes(
                query=query if attempt == 1 else None,
                session_id=current_session,
                item_id=item_id,
            )
        finally:
            stop_ka[0] = True

        final_output = output
        if new_session and new_session != current_session:
            _log(f"Session rotated: {current_session} → {new_session}")
            current_session = new_session

        if completed:
            _log(f"Hermes completed after {attempt} attempt(s)")
            break

        # Not completed — check if it's worth resuming
        _log(f"Hermes exited incomplete (attempt {attempt}). Last output: {output[:200]}")
        if attempt < MAX_RESUME_ATTEMPTS:
            _log(f"Resuming session {current_session} in 5s...")
            time.sleep(5)
        else:
            _log("Max resume attempts reached — giving up")

    if item_id:
        if completed:
            _complete(item_id, final_output)
        else:
            _fail(item_id, f"Hermes did not complete after {attempt} attempts. Last: {final_output[:500]}")

    _log(f"Task done: completed={completed}")


# ── Queue poller ─────────────────────────────────────────────────────────────

def poll_queue() -> None:
    """Continuously poll /api/queue for hermes-appropriate tasks."""
    _log(f"Starting queue poll (agent={AGENT_NAME}, hub={CCC_URL})")
    while True:
        try:
            data = _curl("GET", "/api/queue")
            if data:
                items = data.get("items", [])
                for item in items:
                    if item.get("status") != "pending":
                        continue
                    assignee = item.get("assignee", "")
                    if assignee not in (AGENT_NAME, "all", ""):
                        continue
                    tags = set(item.get("tags", []))
                    preferred = item.get("preferred_executor", "")
                    # Skip tasks explicitly reserved for claude CLI
                    if preferred == "claude_cli" or tags & {"claude", "claude_cli"}:
                        continue
                    item_id = item["id"]
                    title = item.get("title", "?")[:60]
                    _log(f"Found hermes task: {item_id} — {title}")
                    query = f"{item.get('title', '')}\n\n{item.get('description', '')}"
                    run_task(query=query, item_id=item_id)
        except Exception as e:
            _log(f"Poll error: {e}")
        time.sleep(POLL_INTERVAL)


# ── CLI ───────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="CCC hermes task driver")
    parser.add_argument("--item", help="Queue item ID to claim and execute")
    parser.add_argument("--query", help="Task query (used with --item or alone)")
    parser.add_argument("--resume", help="Resume an existing hermes session ID")
    parser.add_argument("--poll", action="store_true", help="Poll queue continuously")
    args = parser.parse_args()

    if args.poll:
        poll_queue()
    elif args.resume:
        run_task(query=args.query, item_id=args.item, session_id=args.resume)
    elif args.query or args.item:
        run_task(query=args.query, item_id=args.item)
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
