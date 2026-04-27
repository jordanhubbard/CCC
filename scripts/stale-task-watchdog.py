#!/usr/bin/env python3
"""
ACC Stale Task Watchdog — detects agents that have gone idle or offline
while still holding claimed/in-progress workqueue items.

NOTE: Enforcement is now handled natively by acc-server (Rust):
  - Stale claim expiry: dispatch.rs auto-unclaims tasks past claim_expires_at
  - Offline agent detection: routes/watchdog.rs emits agent.abandoned_work events
  - Active dispatch: dispatch.rs continuously re-routes unclaimed tasks
This script remains as a read-only audit tool for cron-fleet-monitor reporting.

Runs periodically (via Hermes cron or as part of cron-fleet-monitor).
Outputs a JSON report to stdout.

Checks:
  1. Stale claims — tasks in-progress but claimedAt exceeds threshold
  2. Offline agents with claimed tasks — agent offline but holds work items
  3. Idle agents with pending assigned tasks — agent online but not claiming
     items explicitly assigned to them
  4. Unclaimed old tasks — pending items older than age threshold

Thresholds are configurable via env vars:
  STALE_CLAIM_MINUTES     — in-progress with no keepalive (default: 30)
  OFFLINE_GRACE_MINUTES   — agent offline before alerting (default: 10)
  UNCLAIMED_AGE_HOURS     — pending item age before flagging (default: 24)
  BUSINESS_HOURS_START    — hour (PT) to start monitoring (default: 8)
  BUSINESS_HOURS_END      — hour (PT) to stop monitoring (default: 22)
  WATCHDOG_DRY_RUN        — if "true", report only, no auto-unclaim
"""

import json
import os
import sys
import urllib.request
import urllib.error
from datetime import datetime, timezone, timedelta

# ── Config ─────────────────────────────────────────────────────────
def load_env(path):
    if not os.path.exists(path):
        return
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if '=' in line:
                k, v = line.split('=', 1)
                if k not in os.environ:
                    os.environ[k] = v

load_env(os.path.expanduser("~/.ccc/.env"))
load_env(os.path.expanduser("~/.hermes/.env"))

CCC_URL = os.environ.get("CCC_URL", "http://localhost:8789")
CCC_TOKEN = os.environ.get("CCC_AGENT_TOKEN", "")

# Thresholds
STALE_CLAIM_MINUTES = int(os.environ.get("STALE_CLAIM_MINUTES", "30"))
OFFLINE_GRACE_MINUTES = int(os.environ.get("OFFLINE_GRACE_MINUTES", "10"))
UNCLAIMED_AGE_HOURS = int(os.environ.get("UNCLAIMED_AGE_HOURS", "24"))
BUSINESS_HOURS_START = int(os.environ.get("BUSINESS_HOURS_START", "8"))
BUSINESS_HOURS_END = int(os.environ.get("BUSINESS_HOURS_END", "22"))
DRY_RUN = os.environ.get("WATCHDOG_DRY_RUN", "true").lower() == "true"

# Priority-specific stale claim thresholds (minutes)
PRIORITY_STALE_THRESHOLDS = {
    "urgent": 15,
    "high": 30,
    "medium": 45,
    "normal": 30,
    "low": 120,
    "idea": None,  # ideas don't go stale
}

# Executor-specific stale thresholds (minutes) — matches acc-server logic
EXECUTOR_STALE_THRESHOLDS = {
    "claude_cli": 45,
    "gpu": 120,
    "llm_server": 60,
}


def http_get(url, timeout=10):
    """GET with auth, returns parsed JSON or None."""
    headers = {}
    if CCC_TOKEN:
        headers["Authorization"] = f"Bearer {CCC_TOKEN}"
    req = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode('utf-8', errors='replace'))
    except Exception as e:
        print(f"ERROR fetching {url}: {e}", file=sys.stderr)
        return None


def http_post(url, data, timeout=10):
    """POST JSON with auth, returns parsed JSON or None."""
    headers = {"Content-Type": "application/json"}
    if CCC_TOKEN:
        headers["Authorization"] = f"Bearer {CCC_TOKEN}"
    body = json.dumps(data).encode()
    req = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode('utf-8', errors='replace'))
    except Exception as e:
        print(f"ERROR posting {url}: {e}", file=sys.stderr)
        return None


def parse_ts(ts_str):
    """Parse ISO-8601 timestamp to datetime (UTC)."""
    if not ts_str:
        return None
    try:
        # Handle various formats
        ts_str = ts_str.replace("Z", "+00:00")
        dt = datetime.fromisoformat(ts_str)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone(timezone.utc)
    except Exception:
        return None


def is_business_hours():
    """Check if current time is within business hours (PT)."""
    pt = timezone(timedelta(hours=-7))  # PDT; adjust to -8 for PST
    now_pt = datetime.now(pt)
    return BUSINESS_HOURS_START <= now_pt.hour < BUSINESS_HOURS_END


def get_stale_threshold_minutes(item):
    """Get the stale threshold for an item based on priority and executor."""
    priority = item.get("priority", "normal")
    executor = item.get("preferred_executor", "")

    # Priority threshold
    prio_threshold = PRIORITY_STALE_THRESHOLDS.get(priority, STALE_CLAIM_MINUTES)
    if prio_threshold is None:
        return None  # ideas never go stale

    # Executor threshold
    exec_threshold = EXECUTOR_STALE_THRESHOLDS.get(executor, STALE_CLAIM_MINUTES)

    # Use the more generous of the two
    return max(prio_threshold, exec_threshold)


# ── Checks ─────────────────────────────────────────────────────────

def check_stale_claims(queue_items, agents_map, now):
    """Find in-progress tasks with stale claims."""
    alerts = []
    for item in queue_items:
        if item.get("status") != "in-progress":
            continue

        claimed_by = item.get("claimedBy")
        claimed_at = parse_ts(item.get("claimedAt"))
        keepalive_at = parse_ts(item.get("keepaliveAt"))

        if not claimed_by or not claimed_at:
            continue

        threshold = get_stale_threshold_minutes(item)
        if threshold is None:
            continue

        # Use keepalive if available, otherwise claimedAt
        last_activity = keepalive_at or claimed_at
        age_minutes = (now - last_activity).total_seconds() / 60

        if age_minutes > threshold:
            # Check if the agent is even online
            agent = agents_map.get(claimed_by, {})
            agent_online = agent.get("online", False)

            alerts.append({
                "type": "stale_claim",
                "severity": "high" if not agent_online else "medium",
                "task_id": item.get("id"),
                "title": item.get("title", "")[:80],
                "claimed_by": claimed_by,
                "agent_online": agent_online,
                "claimed_minutes_ago": round(age_minutes),
                "threshold_minutes": threshold,
                "priority": item.get("priority", "normal"),
                "last_activity": (keepalive_at or claimed_at).isoformat(),
            })

    return alerts


def check_offline_agents_with_work(queue_items, agents_map, now):
    """Find agents that are offline but still have claimed items."""
    # Build map of agent -> their claimed items
    agent_claims = {}
    for item in queue_items:
        if item.get("status") == "in-progress" and item.get("claimedBy"):
            agent = item["claimedBy"]
            agent_claims.setdefault(agent, []).append(item)

    alerts = []
    for agent_name, items in agent_claims.items():
        agent = agents_map.get(agent_name, {})
        if agent.get("online", True):
            continue  # Agent is online, fine

        last_seen = parse_ts(agent.get("lastSeen"))
        offline_minutes = (now - last_seen).total_seconds() / 60 if last_seen else 999

        if offline_minutes > OFFLINE_GRACE_MINUTES:
            alerts.append({
                "type": "offline_with_claims",
                "severity": "high",
                "agent": agent_name,
                "offline_minutes": round(offline_minutes),
                "claimed_task_count": len(items),
                "tasks": [
                    {"id": i.get("id"), "title": i.get("title", "")[:60]}
                    for i in items
                ],
                "last_seen": last_seen.isoformat() if last_seen else None,
            })

    return alerts


def check_unclaimed_old_tasks(queue_items, now):
    """Find pending (non-idea) tasks that nobody has claimed in a long time."""
    alerts = []
    threshold = timedelta(hours=UNCLAIMED_AGE_HOURS)

    # Priority-specific unclaimed thresholds
    prio_hours = {
        "urgent": 1,
        "high": 6,
        "medium": 24,
        "normal": 24,
        "low": 72,
    }

    for item in queue_items:
        if item.get("status") != "pending":
            continue
        priority = item.get("priority", "normal")
        if priority in ("idea", "incubating"):
            continue

        created = parse_ts(item.get("created"))
        if not created:
            continue

        hours_threshold = prio_hours.get(priority, UNCLAIMED_AGE_HOURS)
        age_hours = (now - created).total_seconds() / 3600

        if age_hours > hours_threshold:
            alerts.append({
                "type": "unclaimed_old",
                "severity": "medium" if priority in ("urgent", "high") else "low",
                "task_id": item.get("id"),
                "title": item.get("title", "")[:80],
                "priority": priority,
                "assignee": item.get("assignee", "any"),
                "age_hours": round(age_hours, 1),
                "threshold_hours": hours_threshold,
                "created": created.isoformat(),
            })

    return alerts


def check_blocked_tasks(queue_items):
    """Find blocked tasks that may need manual intervention."""
    alerts = []
    for item in queue_items:
        if item.get("status") != "blocked":
            continue

        alerts.append({
            "type": "blocked_task",
            "severity": "medium",
            "task_id": item.get("id"),
            "title": item.get("title", "")[:80],
            "priority": item.get("priority", "normal"),
            "blocked_reason": item.get("blockedReason", "unknown")[:200],
            "attempts": item.get("attempts", 0),
            "max_attempts": item.get("maxAttempts", 3),
        })

    return alerts


def auto_unclaim_stale(alerts):
    """Optionally release stale claims so other agents can pick them up."""
    if DRY_RUN:
        return []

    released = []
    for alert in alerts:
        if alert["type"] != "stale_claim":
            continue
        if alert["severity"] != "high":  # Only auto-release truly stale ones
            continue

        task_id = alert["task_id"]
        result = http_post(f"{CCC_URL}/api/item/{task_id}/stale-reset", {})
        if result:
            released.append(task_id)

    return released


# ── Main ───────────────────────────────────────────────────────────

def main():
    now = datetime.now(timezone.utc)
    report = {
        "timestamp": now.isoformat(),
        "business_hours": is_business_hours(),
        "dry_run": DRY_RUN,
    }

    # 1. Fetch agent data
    agents_data = http_get(f"{CCC_URL}/api/agents")
    if not agents_data:
        report["error"] = "Failed to fetch agents"
        print(json.dumps(report, indent=2))
        sys.exit(1)

    agents_list = agents_data.get("agents", [])
    agents_map = {
        a.get("name"): a for a in agents_list
        if not a.get("decommissioned", False)
    }
    report["agents_online"] = sum(1 for a in agents_map.values() if a.get("online"))
    report["agents_total"] = len(agents_map)

    # 2. Fetch stale tasks from server (it has its own threshold logic)
    stale_data = http_get(f"{CCC_URL}/api/queue/stale")
    server_stale = stale_data.get("stale", []) if stale_data else []
    report["server_stale_count"] = len(server_stale)

    # 3. Fetch claimed tasks
    claimed_data = http_get(f"{CCC_URL}/api/queue/claimed")
    claimed = claimed_data.get("claimed", []) if claimed_data else []
    report["claimed_count"] = len(claimed)

    # 4. Fetch full queue (active items only — NOT completed)
    queue_data = http_get(f"{CCC_URL}/api/queue?exclude_completed=true")
    if not queue_data:
        # Fallback: use stale + claimed data we already have
        queue_items = server_stale + claimed
    else:
        queue_items = queue_data.get("items", [])
        if not queue_items and isinstance(queue_data, list):
            queue_items = queue_data

    # 5. Run all checks
    all_alerts = []

    # Stale claims (our enriched version)
    stale_alerts = check_stale_claims(queue_items, agents_map, now)
    all_alerts.extend(stale_alerts)

    # Offline agents with work
    offline_alerts = check_offline_agents_with_work(queue_items, agents_map, now)
    all_alerts.extend(offline_alerts)

    # Unclaimed old tasks (only during business hours)
    if is_business_hours():
        unclaimed_alerts = check_unclaimed_old_tasks(queue_items, now)
        all_alerts.extend(unclaimed_alerts)

    # Blocked tasks always checked
    blocked_alerts = check_blocked_tasks(queue_items)
    all_alerts.extend(blocked_alerts)

    # 6. Auto-recovery (if enabled)
    released = auto_unclaim_stale(stale_alerts)
    report["auto_released"] = released

    # 7. Sort alerts by severity
    severity_order = {"critical": 0, "high": 1, "medium": 2, "low": 3}
    all_alerts.sort(key=lambda a: severity_order.get(a.get("severity", "low"), 9))

    report["alerts"] = all_alerts
    report["alert_count"] = len(all_alerts)
    report["alert_summary"] = {
        "stale_claims": len(stale_alerts),
        "offline_with_claims": len(offline_alerts),
        "unclaimed_old": len([a for a in all_alerts if a["type"] == "unclaimed_old"]),
        "blocked": len(blocked_alerts),
    }

    # 8. Overall health
    high_severity = [a for a in all_alerts if a.get("severity") in ("critical", "high")]
    report["healthy"] = len(high_severity) == 0
    report["needs_attention"] = len(high_severity) > 0

    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
