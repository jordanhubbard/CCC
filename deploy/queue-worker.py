#!/usr/bin/env python3
"""
queue-worker.py — Persistent CCC queue worker daemon.

Polls /api/queue every 60s, claims and executes pending items assigned to this
agent via `claude -p`, posts keepalives and results back to the hub.

Usage (direct):   python3 queue-worker.py
Supervisord:      command=python3 /home/.../queue-worker.py
Systemd:          ExecStart=/usr/bin/python3 AGENT_HOME/.ccc/workspace/deploy/queue-worker.py
"""

import json
import logging
import os
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

# ── Configuration ────────────────────────────────────────────────────────────

CCC_DIR = Path(os.environ.get("HOME", "/home")) / ".ccc"
ENV_FILE = CCC_DIR / ".env"
LOG_FILE = CCC_DIR / "logs" / "queue-worker.log"
QUENCH_FILE = CCC_DIR / "quench"
WORKSPACE = CCC_DIR / "workspace"

POLL_INTERVAL_IDLE = 60       # seconds between queue checks when no work found
POLL_INTERVAL_BUSY = 5        # seconds between checks right after completing a task
KEEPALIVE_INTERVAL = 25 * 60  # 25 min (TTL is 2h for claude_cli, keepalive margin)
CLAUDE_TIMEOUT = 7200         # 2 hours max per task
HTTP_TIMEOUT = 15             # seconds for API calls


# ── Logging ──────────────────────────────────────────────────────────────────

LOG_FILE.parent.mkdir(parents=True, exist_ok=True)
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%dT%H:%M:%SZ",
    handlers=[
        logging.FileHandler(LOG_FILE),
        logging.StreamHandler(sys.stdout),
    ],
)
log = logging.getLogger("queue-worker")

# Override basicConfig to use UTC
logging.Formatter.converter = time.gmtime


# ── Load .env ────────────────────────────────────────────────────────────────

def load_env():
    if ENV_FILE.exists():
        with open(ENV_FILE) as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    key, _, val = line.partition("=")
                    # Strip quotes
                    val = val.strip().strip("'\"")
                    os.environ.setdefault(key.strip(), val)

load_env()

AGENT_NAME = os.environ.get("AGENT_NAME", "")
CCC_URL = os.environ.get("CCC_URL", "").rstrip("/")
CCC_AGENT_TOKEN = os.environ.get("CCC_AGENT_TOKEN", "")

if not AGENT_NAME or not CCC_URL or not CCC_AGENT_TOKEN:
    log.error("AGENT_NAME, CCC_URL, and CCC_AGENT_TOKEN must be set in ~/.ccc/.env")
    sys.exit(1)


# ── HTTP helpers ─────────────────────────────────────────────────────────────

def _curl(method: str, path: str, body: dict | None = None) -> dict | None:
    """Execute a curl request to the CCC hub. Returns parsed JSON or None."""
    url = f"{CCC_URL}{path}"
    cmd = [
        "curl", "-sf", "--max-time", str(HTTP_TIMEOUT),
        "-X", method,
        "-H", f"Authorization: Bearer {CCC_AGENT_TOKEN}",
        "-H", "Content-Type: application/json",
    ]
    if body is not None:
        cmd += ["-d", json.dumps(body)]
    cmd.append(url)

    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=HTTP_TIMEOUT + 5)
        if result.returncode != 0 or not result.stdout.strip():
            return None
        return json.loads(result.stdout)
    except (subprocess.TimeoutExpired, json.JSONDecodeError, Exception) as e:
        log.debug(f"curl {method} {path} failed: {e}")
        return None


def get_queue() -> list[dict]:
    data = _curl("GET", "/api/queue")
    if not data:
        return []
    return data.get("items", [])


def claim_item(item_id: str) -> bool:
    resp = _curl("POST", f"/api/item/{item_id}/claim", {
        "agent": AGENT_NAME,
        "note": "queue-worker claiming",
    })
    return resp is not None


def post_keepalive(item_id: str, note: str = "still working") -> None:
    _curl("POST", f"/api/item/{item_id}/keepalive", {
        "agent": AGENT_NAME,
        "note": note,
    })


def post_complete(item_id: str, result: str) -> None:
    _curl("POST", f"/api/item/{item_id}/complete", {
        "agent": AGENT_NAME,
        "result": result[:4000],  # cap result size
        "resolution": result[:4000],
    })


def post_fail(item_id: str, reason: str) -> None:
    _curl("POST", f"/api/item/{item_id}/fail", {
        "agent": AGENT_NAME,
        "reason": reason[:2000],
    })


def post_comment(item_id: str, text: str) -> None:
    _curl("POST", f"/api/item/{item_id}/comment", {
        "text": text[:2000],
        "author": AGENT_NAME,
    })


def check_best_agent(executor_hint: str) -> str | None:
    """Ask hub who should handle this task. Returns agent name or None."""
    resp = _curl("GET", f"/api/agents/best?task={executor_hint}")
    if resp:
        return resp.get("agent") or resp.get("name")
    return None


# ── Quench check ─────────────────────────────────────────────────────────────

def is_quenched() -> bool:
    if not QUENCH_FILE.exists():
        return False
    try:
        until_str = QUENCH_FILE.read_text().strip()
        until = datetime.fromisoformat(until_str.replace("Z", "+00:00"))
        now = datetime.now(timezone.utc)
        if now < until:
            return True
        else:
            QUENCH_FILE.unlink(missing_ok=True)
            return False
    except Exception:
        return False


# ── Agent instructions loader ─────────────────────────────────────────────────

def load_agent_instructions() -> str:
    """Load the agent-specific or generic WORKQUEUE_AGENT.md as a system prompt."""
    # Try agent-specific first
    candidates = [
        WORKSPACE / "workqueue" / f"WORKQUEUE_AGENT_{AGENT_NAME.upper()}.md",
        WORKSPACE / "workqueue" / "WORKQUEUE_AGENT.md",
    ]
    for path in candidates:
        if path.exists():
            return path.read_text()
    return (
        f"You are the workqueue agent for {AGENT_NAME}. "
        "Process the assigned task completely. "
        "Report your result clearly and concisely."
    )


# ── Task execution ────────────────────────────────────────────────────────────

def build_task_prompt(item: dict, instructions: str) -> str:
    """Build the full prompt for claude given a queue item."""
    parts = [
        f"# Queue Item: {item['id']}",
        f"**Title:** {item.get('title', '(no title)')}",
        f"**Priority:** {item.get('priority', 'normal')}",
        f"**Assignee:** {item.get('assignee', AGENT_NAME)}",
        "",
        "## Description",
        item.get("description", "(no description)"),
    ]
    if item.get("notes"):
        parts += ["", "## Notes / Context", item["notes"]]
    if item.get("tags"):
        parts += ["", f"**Tags:** {', '.join(item['tags'])}"]
    parts += [
        "",
        "---",
        "## Your Task",
        "Complete the above work item. When done, summarize what you did as your final output.",
        "The summary will be posted as the item result.",
    ]
    return "\n".join(parts)


def run_claude(prompt: str, item_id: str) -> tuple[str, int]:
    """
    Run claude -p with the given prompt.
    Sends keepalives in background thread during execution.
    Returns (output, exit_code).
    """
    # Keepalive thread
    stop_keepalive = threading.Event()

    def keepalive_loop():
        while not stop_keepalive.wait(KEEPALIVE_INTERVAL):
            post_keepalive(item_id, "claude still working")
            log.info(f"[{item_id}] keepalive sent")

    ka_thread = threading.Thread(target=keepalive_loop, daemon=True)
    ka_thread.start()

    try:
        result = subprocess.run(
            ["claude", "-p", prompt],
            capture_output=True,
            text=True,
            timeout=CLAUDE_TIMEOUT,
            env={**os.environ},
        )
        output = result.stdout.strip() or result.stderr.strip() or "(no output)"
        exit_code = result.returncode
        if result.returncode != 0 and result.stderr:
            output = f"{output}\n\nSTDERR:\n{result.stderr.strip()}"
    except subprocess.TimeoutExpired:
        output = f"[timed out after {CLAUDE_TIMEOUT}s]"
        exit_code = 124
    except FileNotFoundError:
        output = "ERROR: 'claude' CLI not found in PATH"
        exit_code = 127
    except Exception as e:
        output = f"ERROR: {e}"
        exit_code = 1
    finally:
        stop_keepalive.set()

    return output, exit_code


# ── Item selector ─────────────────────────────────────────────────────────────

def select_item(items: list[dict]) -> dict | None:
    """
    Pick the best claimable item for this agent.
    Priority order: urgent > high > normal > low
    Skips: non-pending, deferred, blocked, jkh-assigned, items assigned to other agents.
    For 'all' items: checks /api/agents/best to route correctly.
    """
    priority_order = {"urgent": 0, "high": 1, "normal": 2, "medium": 2, "low": 3, "idea": 99}

    candidates = []
    for item in items:
        status = item.get("status", "")
        assignee = item.get("assignee", "")
        if status != "pending":
            continue
        if assignee == "jkh":
            continue  # human-assigned items
        if assignee not in (AGENT_NAME, "all", ""):
            continue  # belongs to another agent

        # For 'all' items: check capability routing
        if assignee == "all":
            executor = item.get("preferred_executor") or item.get("tags", [""])[0]
            best = check_best_agent(executor)
            if best and best != AGENT_NAME:
                log.debug(f"Skipping {item['id']} (best agent is {best})")
                continue

        candidates.append(item)

    if not candidates:
        return None

    # Sort by priority then created timestamp
    candidates.sort(key=lambda x: (
        priority_order.get(x.get("priority", "normal"), 2),
        x.get("created", ""),
    ))
    return candidates[0]


# ── Main loop ─────────────────────────────────────────────────────────────────

def main():
    log.info(f"Starting queue-worker (agent={AGENT_NAME}, hub={CCC_URL})")
    instructions = load_agent_instructions()
    poll_interval = POLL_INTERVAL_IDLE

    while True:
        if is_quenched():
            log.info("Quenched — skipping this cycle")
            time.sleep(POLL_INTERVAL_IDLE)
            continue

        try:
            items = get_queue()
        except Exception as e:
            log.warning(f"Failed to fetch queue: {e}")
            time.sleep(POLL_INTERVAL_IDLE)
            continue

        item = select_item(items)
        if item is None:
            log.debug("No claimable items — sleeping")
            time.sleep(poll_interval)
            poll_interval = POLL_INTERVAL_IDLE
            continue

        item_id = item["id"]
        title = item.get("title", "?")
        priority = item.get("priority", "normal")

        # Claim
        if not claim_item(item_id):
            log.info(f"[{item_id}] Claim rejected (409 or error) — backing off")
            time.sleep(POLL_INTERVAL_BUSY)
            continue

        log.info(f"[{item_id}] Claimed [{priority}] {title[:60]}")

        # Alert peers for urgent items
        if priority == "urgent":
            _curl("POST", "/bus/send", {
                "from": AGENT_NAME,
                "to": "all",
                "type": "text",
                "subject": "ops",
                "body": f"[URGENT] {title} assigned to {AGENT_NAME} — working now",
            })

        # Build prompt and run
        prompt = build_task_prompt(item, instructions)
        post_comment(item_id, f"{AGENT_NAME} starting execution via queue-worker")

        log.info(f"[{item_id}] Invoking claude...")
        output, exit_code = run_claude(prompt, item_id)
        log.info(f"[{item_id}] claude exited {exit_code} ({len(output)} chars output)")

        # Post result
        if exit_code == 0:
            post_complete(item_id, output)
            log.info(f"[{item_id}] Completed OK")
        else:
            post_fail(item_id, f"exit_code={exit_code}\n{output[:1000]}")
            log.info(f"[{item_id}] Failed (exit={exit_code})")

        # Poll more aggressively right after completing a task
        poll_interval = POLL_INTERVAL_BUSY


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        log.info("Interrupted — exiting")
        sys.exit(0)
