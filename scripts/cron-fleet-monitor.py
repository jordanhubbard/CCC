#!/usr/bin/env python3
"""
CCC Fleet Monitor — combined health check + Slack ingestion.
Runs every 10 minutes via Hermes cron.
Outputs a compact summary for the cron delivery target.
"""

import subprocess, sys, json, os

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))

# ── Severity emoji map ─────────────────────────────────────────────
SEVERITY_EMOJI = {"critical": "🔴", "high": "🟠", "medium": "🟡", "low": "⚪"}

def run_script(name):
    path = os.path.join(SCRIPTS_DIR, name)
    try:
        r = subprocess.run(
            [sys.executable, path],
            capture_output=True, text=True, timeout=120,
            cwd=os.path.dirname(SCRIPTS_DIR),
        )
        return r.stdout.strip(), r.stderr.strip(), r.returncode
    except subprocess.TimeoutExpired:
        return "", f"TIMEOUT after 120s", 1
    except Exception as e:
        return "", str(e), 1


def summarize_health(raw):
    try:
        data = json.loads(raw)
    except:
        return f"Health check output parse error: {raw[:200]}"

    lines = []
    # Services
    down = [s for s in data.get("services", []) if not s["ok"]]
    total = len(data.get("services", []))
    if down:
        lines.append(f"SERVICES: {total - len(down)}/{total} up")
        for s in down:
            lines.append(f"  DOWN: {s['name']} — {s.get('error', '?')}")
    else:
        lines.append(f"Services: all {total} up")

    # Tokenhub providers (if present)
    provs = data.get("tokenhub_providers", [])
    if provs:
        pdown = [p for p in provs if not p.get("ok")]
        if pdown:
            lines.append(f"PROVIDERS: {len(provs) - len(pdown)}/{len(provs)} healthy")
            for p in pdown:
                lines.append(f"  DOWN: {p['name']}")
        else:
            lines.append(f"Providers: all {len(provs)} healthy")

    # Agents
    agents = data.get("agents", [])
    offline = [a for a in agents if not a.get("online")]
    if offline:
        names = ", ".join(a["name"] for a in offline)
        lines.append(f"AGENTS OFFLINE: {names}")

    # Remote AccFS access
    remote = data.get("remote_accfs", [])
    if remote:
        rfail = [r for r in remote if not r.get("ok")]
        if rfail:
            for r in rfail:
                lines.append(f"  ACCFS UNREACHABLE from {r['name']}: {r.get('error', '?')}")
        else:
            lines.append(f"Remote AccFS: all {len(remote)} nodes OK")

    return "\n".join(lines)


def summarize_watchdog(raw):
    try:
        data = json.loads(raw)
    except Exception:
        return f"Watchdog parse error: {raw[:200]}"

    alerts = data.get("alerts", [])
    alert_count = data.get("alert_count", 0)
    summary = data.get("alert_summary", {})
    healthy = data.get("healthy", True)
    agents_online = data.get("agents_online", "?")
    agents_total = data.get("agents_total", "?")
    released = data.get("auto_released", [])

    if alert_count == 0:
        return f"Watchdog: ✅ all clear ({agents_online}/{agents_total} agents online)"

    lines = []
    if not healthy:
        lines.append(f"⚠️ WATCHDOG ALERT — {alert_count} issue(s) detected")
    else:
        lines.append(f"Watchdog: {alert_count} notice(s)")

    # Stale claims
    if summary.get("stale_claims", 0) > 0:
        stale_alerts = [a for a in alerts if a["type"] == "stale_claim"]
        for a in stale_alerts:
            emoji = SEVERITY_EMOJI.get(a.get("severity", "low"), "⚪")
            agent_status = "OFFLINE" if not a.get("agent_online") else "online"
            lines.append(
                f"  {emoji} STALE: {a['claimed_by']} ({agent_status}) "
                f"holding `{a['task_id']}` for {a['claimed_minutes_ago']}min "
                f"(threshold: {a['threshold_minutes']}min)"
            )

    # Offline agents with claims
    if summary.get("offline_with_claims", 0) > 0:
        offline_alerts = [a for a in alerts if a["type"] == "offline_with_claims"]
        for a in offline_alerts:
            task_ids = ", ".join(t["id"] for t in a.get("tasks", []))
            lines.append(
                f"  🟠 OFFLINE: {a['agent']} offline {a['offline_minutes']}min "
                f"with {a['claimed_task_count']} claimed task(s): {task_ids}"
            )

    # Unclaimed old
    if summary.get("unclaimed_old", 0) > 0:
        old_alerts = [a for a in alerts if a["type"] == "unclaimed_old"]
        for a in old_alerts[:3]:  # Cap at 3 to avoid spam
            lines.append(
                f"  🟡 UNCLAIMED: `{a['task_id']}` ({a['priority']}) "
                f"pending {a['age_hours']}h — assigned to {a.get('assignee', 'any')}"
            )
        if len(old_alerts) > 3:
            lines.append(f"  ... and {len(old_alerts) - 3} more unclaimed items")

    # Blocked
    if summary.get("blocked", 0) > 0:
        blocked_alerts = [a for a in alerts if a["type"] == "blocked_task"]
        for a in blocked_alerts[:3]:
            lines.append(
                f"  🟡 BLOCKED: `{a['task_id']}` — {a['title'][:50]}"
            )
        if len(blocked_alerts) > 3:
            lines.append(f"  ... and {len(blocked_alerts) - 3} more blocked")

    # Auto-released
    if released:
        lines.append(f"  ♻️ Auto-released {len(released)} stale claim(s): {', '.join(released)}")

    return "\n".join(lines)


def summarize_ingest(raw):
    try:
        data = json.loads(raw)
    except:
        return f"Ingest output: {raw[:200]}"

    total = data.get("total_ingested", 0)
    errors = data.get("errors", [])
    channels = data.get("channels", {})
    parts = []
    for ch, info in channels.items():
        n = info.get("new", 0)
        if n > 0:
            parts.append(f"{ch}={n}")

    if not parts:
        return "Ingest: no new messages"

    line = f"Ingested {total} msgs ({', '.join(parts)})"
    if errors:
        line += f" [{len(errors)} errors]"
    return line


def _watchdog_has_alerts(raw):
    """True when the watchdog detected anything worth surfacing."""
    try:
        data = json.loads(raw)
    except Exception:
        return True  # parse failure is itself noteworthy
    return data.get("alert_count", 0) > 0 or not data.get("healthy", True)


def _health_has_alerts(raw):
    """True when fleet-health-check.py reported something down/offline."""
    try:
        data = json.loads(raw)
    except Exception:
        return True
    if any(not s.get("ok", True) for s in data.get("services", [])):
        return True
    if any(not p.get("ok", True) for p in data.get("tokenhub_providers", [])):
        return True
    if any(not a.get("online", True) for a in data.get("agents", [])):
        return True
    if any(not r.get("ok", True) for r in data.get("remote_accfs", [])):
        return True
    return False


if __name__ == "__main__":
    # Silence-on-success: this script is invoked by hermes' cronjob tool,
    # which posts any non-empty stdout to Slack. To stop spamming the
    # channel with "all clear" every interval, we now emit output ONLY
    # when something needs attention. Real problems still surface; the
    # uneventful 99% of runs go silent.
    output_lines = []

    stdout, stderr, rc = run_script("fleet-health-check.py")
    if rc != 0:
        output_lines.append(f"Health check FAILED (rc={rc}): {stderr[:200]}")
    elif stdout and _health_has_alerts(stdout):
        output_lines.append(summarize_health(stdout))

    stdout, stderr, rc = run_script("stale-task-watchdog.py")
    if rc != 0:
        output_lines.append(f"Watchdog FAILED (rc={rc}): {stderr[:200]}")
    elif stdout and _watchdog_has_alerts(stdout):
        output_lines.append(summarize_watchdog(stdout))

    stdout, stderr, rc = run_script("slack-channel-ingest.py")
    if rc != 0:
        output_lines.append(f"Ingest FAILED (rc={rc}): {stderr[:200]}")
    # Successful ingestion is silent — the messages themselves are the
    # signal; we don't need a meta-summary in #acc-fleet.

    if output_lines:
        print("\n".join(output_lines))
