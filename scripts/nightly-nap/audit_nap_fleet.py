#!/usr/bin/env python3
"""
audit_nap_fleet.py — Verify per-agent short-term .md → holographic storage copy.

Checks each fleet agent against three required evidence criteria:

  1. CONTENT   — The holographic store (Qdrant) contains the agent's recent
                 .md memory content (nightly_nap points within the last N days).
  2. SCHEDULED — The copy is scheduled (a cron entry with the acc-nightly-nap
                 tag exists in the system crontab, or a nap state file exists
                 proving setup was completed).
  3. FIRING    — The copy is actually firing (recent timestamps — last
                 successful nap within STALE_THRESHOLD_DAYS, not stale).

Triage output per agent:
  ✅ PASS  — all three criteria met
  ⚠  WARN  — scheduled but stale / no recent nap (not firing)
  ❌ FAIL  — not scheduled or no Qdrant content found

Exit codes:
  0  all agents pass
  1  one or more agents warn/fail
  2  configuration / credential error

Usage
-----
  # Check all fleet agents (live list from ACC registry):
  python3 audit_nap_fleet.py

  # Check specific agents:
  python3 audit_nap_fleet.py --agents rocky natasha

  # Emit machine-readable JSON:
  python3 audit_nap_fleet.py --json

  # Set stale threshold (default 2 days):
  python3 audit_nap_fleet.py --stale-days 3

  # Show Qdrant content snippets:
  python3 audit_nap_fleet.py --show-content

  # Check only local state files (no Qdrant call — useful offline):
  python3 audit_nap_fleet.py --local-only

Environment variables
---------------------
  AGENT_NAME            If set, defaults --agents to this single agent.
  QDRANT_URL            Qdrant base URL (default: http://localhost:6333)
  TOKENHUB_URL          Tokenhub base URL (default: http://localhost:8090)
  TOKENHUB_API_KEY      Tokenhub bearer key
  HERMES_HOME           Hermes home dir (default: ~/.hermes)

Fleet agent list resolution (same priority chain as schedule_nap_cron.py):
  1. ACC registry API  — GET $ACC_URL/api/agents/names
  2. agents.json file  — $ACC_DATA_DIR/agents.json
  3. ACC_FLEET_AGENTS  — comma-separated env var
  4. Built-in default  — founding four agents (offline fallback only)
"""

import argparse
import json
import logging
import os
import subprocess
import sys
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

# ── stdlib-only import of shared Qdrant helpers ────────────────────────────
_HERE = Path(__file__).resolve().parent
_QDRANT_PY = _HERE.parent / "qdrant-python"
if str(_QDRANT_PY) not in sys.path:
    sys.path.insert(0, str(_QDRANT_PY))

# ── Fleet agent list — resolved dynamically from schedule_nap_cron ─────────
# Import the same resolution logic used by the scheduler so both scripts
# always operate on exactly the same fleet list from the same sources.
# This replaces the former hardcoded FLEET_AGENTS constant.
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

try:
    from schedule_nap_cron import resolve_known_agents as _resolve_known_agents
    _FLEET_AGENTS, _FLEET_AGENTS_SOURCE = _resolve_known_agents(verbose=False)
except Exception as _e:
    # Absolute last resort — should never happen in practice.
    _FLEET_AGENTS = ["boris", "natasha", "rocky", "bullwinkle"]
    _FLEET_AGENTS_SOURCE = f"built-in default (schedule_nap_cron import failed: {_e})"

# ── Constants ──────────────────────────────────────────────────────────────

COLLECTION_LT = "agent_long_term_memory"
STALE_THRESHOLD_DAYS = 2       # nap older than this → WARN (not firing)
CONTENT_LOOKBACK_DAYS = 7      # how many days back to search Qdrant
CRON_TAG = "# acc-nightly-nap"

logging.basicConfig(
    level=logging.WARNING,
    format="[%(levelname)s] %(message)s",
)
log = logging.getLogger("audit_nap_fleet")


# ── Helpers ────────────────────────────────────────────────────────────────

def get_hermes_home() -> Path:
    val = os.environ.get("HERMES_HOME", "").strip()
    return Path(val) if val else Path.home() / ".hermes"


def load_env_file(path: str) -> None:
    p = Path(path).expanduser()
    if not p.exists():
        return
    with open(p) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            k = k.strip()
            v = v.strip().strip('"').strip("'")
            if k and k not in os.environ:
                os.environ[k] = v


def _state_file_path(agent_name: str) -> Path:
    override = os.environ.get("NAP_STATE_FILE", "").strip()
    if override:
        return Path(override).expanduser()
    hermes_home = get_hermes_home()
    scripts_dir = hermes_home / "scripts"
    return scripts_dir / f".nap_state_{agent_name}.json"


def load_nap_state(agent_name: str) -> dict:
    path = _state_file_path(agent_name)
    if path.exists():
        try:
            with open(path) as f:
                return json.load(f)
        except Exception:
            pass
    return {}


# ── Crontab check ─────────────────────────────────────────────────────────

def _get_crontab() -> str:
    """Return current user's crontab content (empty string on failure)."""
    try:
        result = subprocess.run(
            ["crontab", "-l"],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode == 0:
            return result.stdout
        return ""
    except Exception:
        return ""


def check_cron_installed(agent_name: str, crontab_content: str) -> tuple[bool, str]:
    """Return (is_installed, detail_message).

    Checks both the user crontab and the nap state file (proxy for
    '--install' having been run, even when checking from a different box).
    """
    # Primary: cron line present in live crontab
    if CRON_TAG in crontab_content:
        # Find the line for this agent
        for line in crontab_content.splitlines():
            if CRON_TAG in line and f"AGENT_NAME={agent_name}" in line:
                # Extract the UTC time from the cron schedule
                parts = line.strip().split()
                if len(parts) >= 2:
                    try:
                        minute = int(parts[0])
                        hour = int(parts[1])
                        return True, f"cron installed, fires at {hour:02d}:{minute:02d} UTC daily"
                    except ValueError:
                        pass
                return True, "cron installed (could not parse schedule)"

        # Cron tag present but not for this agent — check state file
        state = load_nap_state(agent_name)
        if state.get("last_successful_date"):
            return True, f"cron tag found (other agent), state file present (ran {state['last_successful_date']})"

        return False, f"cron tag found in crontab but not for agent '{agent_name}'"

    # Fallback: nap state file proves --install was run on the agent's own box
    state = load_nap_state(agent_name)
    if state.get("last_successful_date") or state.get("updated_at"):
        return True, f"no live cron (different box?), but state file exists — was installed on agent's own box"

    return False, "no cron entry and no nap state file found"


# ── Qdrant content check ───────────────────────────────────────────────────

def check_qdrant_content(
    agent_name: str,
    lookback_days: int,
    api_key: str,
    show_content: bool = False,
) -> tuple[bool, str, list[str]]:
    """Return (has_content, detail_message, content_snippets).

    Queries Qdrant for nightly_nap points for this agent within
    the lookback window, using a scroll/filter query (no embedding needed).
    """
    try:
        from qdrant_common import qdrant_post, qdrant_get
    except ImportError:
        return False, "qdrant_common not importable — check scripts/qdrant-python/", []

    today = datetime.now(timezone.utc).date()

    # Date strings for all days in the lookback window
    date_range = []
    for i in range(lookback_days + 1):
        d = today - timedelta(days=i)
        date_range.append(d.isoformat())

    try:
        # Use scroll with a filter — no vector needed
        payload = {
            "filter": {
                "must": [
                    {"key": "agent", "match": {"value": agent_name}},
                    {"key": "chunk_type", "match": {"value": "nightly_nap"}},
                    {
                        "key": "date",
                        "match": {"any": date_range},
                    },
                ]
            },
            "limit": 10,
            "with_payload": True,
            "with_vector": False,
        }
        result = qdrant_post(
            f"/collections/{COLLECTION_LT}/points/scroll",
            payload,
            api_key=api_key,
        )
    except Exception as exc:
        err_str = str(exc)
        if "404" in err_str or "Not found" in err_str.lower():
            return False, f"collection '{COLLECTION_LT}' does not exist yet", []
        if "Connection refused" in err_str or "Failed to connect" in err_str or "refused" in err_str.lower():
            return False, "Qdrant unreachable (connection refused) — is Qdrant running?", []
        return False, f"Qdrant query failed: {exc}", []

    points = result.get("result", {}).get("points", [])
    if not points:
        return False, f"no nightly_nap points found for '{agent_name}' in the last {lookback_days} days", []

    # Find the most recent date among the points
    dates_found = sorted(
        set(p["payload"].get("date", "") for p in points if p.get("payload")),
        reverse=True,
    )
    most_recent = dates_found[0] if dates_found else "?"
    count = len(points)

    snippets = []
    if show_content:
        for p in points[:3]:
            payload_data = p.get("payload", {})
            text = (payload_data.get("text", "") or "").strip()
            if text:
                snippets.append(text[:200] + ("…" if len(text) > 200 else ""))

    detail = f"{count} point(s) found, most recent nap date: {most_recent}"
    return True, detail, snippets


# ── Timestamp / staleness check ────────────────────────────────────────────

def check_firing_recently(agent_name: str, stale_days: int) -> tuple[str, str, str | None]:
    """Return (status, detail, last_date).

    status is one of: 'ok', 'stale', 'never', 'unknown'
    """
    state = load_nap_state(agent_name)
    last_date_str = state.get("last_successful_date", "")
    last_result = state.get("last_run_result", "")

    if not last_date_str:
        return "never", "no successful nap recorded in state file", None

    try:
        last_date = date.fromisoformat(last_date_str)
    except ValueError:
        return "unknown", f"invalid date in state file: {last_date_str!r}", None

    today = datetime.now(timezone.utc).date()
    age_days = (today - last_date).days

    if age_days <= stale_days:
        detail = f"last successful nap: {last_date_str} ({age_days}d ago)"
        if last_result:
            detail += f", result={last_result}"
        return "ok", detail, last_date_str
    else:
        detail = f"last successful nap: {last_date_str} ({age_days}d ago) — STALE (threshold: {stale_days}d)"
        if last_result:
            detail += f", result={last_result}"
        return "stale", detail, last_date_str


# ── Per-agent audit ────────────────────────────────────────────────────────

def audit_agent(
    agent_name: str,
    crontab_content: str,
    stale_days: int,
    lookback_days: int,
    qdrant_api_key: str | None,
    show_content: bool,
    local_only: bool,
) -> dict:
    """Run the full three-criteria audit for one agent.

    Returns a result dict with keys:
      agent, scheduled, firing, content, overall, details, snippets
    """
    result = {
        "agent": agent_name,
        "scheduled": None,    # True/False/None
        "firing": None,       # 'ok'/'stale'/'never'/'unknown'
        "content": None,      # True/False/None
        "overall": None,      # 'pass'/'warn'/'fail'
        "details": {},
        "snippets": [],
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }

    # ── Criterion 1: Scheduled ────────────────────────────────────────────
    sched_ok, sched_detail = check_cron_installed(agent_name, crontab_content)
    result["scheduled"] = sched_ok
    result["details"]["scheduled"] = sched_detail

    # ── Criterion 2: Firing (recent timestamps) ───────────────────────────
    fire_status, fire_detail, last_date = check_firing_recently(agent_name, stale_days)
    result["firing"] = fire_status
    result["details"]["firing"] = fire_detail
    result["last_successful_date"] = last_date

    # ── Criterion 3: Content in holographic store ─────────────────────────
    if local_only or qdrant_api_key is None:
        result["content"] = None
        result["details"]["content"] = "skipped (--local-only or no Qdrant key)"
    else:
        content_ok, content_detail, snippets = check_qdrant_content(
            agent_name, lookback_days, qdrant_api_key, show_content
        )
        result["content"] = content_ok
        result["details"]["content"] = content_detail
        result["snippets"] = snippets

    # ── Overall verdict ───────────────────────────────────────────────────
    # PASS: scheduled + firing ok + content present (or content skipped)
    # WARN: scheduled but stale / content missing
    # FAIL: not scheduled, or firing=never with no content
    content_ok_or_skipped = result["content"] is None or result["content"] is True

    if (
        result["scheduled"]
        and result["firing"] == "ok"
        and content_ok_or_skipped
    ):
        result["overall"] = "pass"
    elif (
        not result["scheduled"]
        and result["firing"] in ("never", "unknown")
        and result["content"] is False
    ):
        result["overall"] = "fail"
    elif not result["scheduled"] and result["firing"] in ("never", "unknown"):
        result["overall"] = "fail"
    else:
        result["overall"] = "warn"

    return result


# ── Report formatting ──────────────────────────────────────────────────────

_STATUS_ICON = {
    "pass": "✅",
    "warn": "⚠ ",
    "fail": "❌",
}

_FIRING_ICON = {
    "ok": "✅",
    "stale": "⚠ ",
    "never": "❌",
    "unknown": "❓",
}


def format_report(results: list[dict], show_content: bool, agents_source: str) -> str:
    lines = []
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    lines.append(f"\n{'='*70}")
    lines.append(f"  Fleet Audit: per-agent short-term .md → holographic storage copy")
    lines.append(f"  Generated : {today}")
    lines.append(f"  Fleet list: {agents_source}")
    lines.append(f"{'='*70}\n")

    for r in results:
        agent = r["agent"]
        overall = r["overall"]
        icon = _STATUS_ICON.get(overall, "?")

        lines.append(f"  {icon} {agent.upper():<14} [{overall.upper()}]")
        lines.append(f"     {'─'*50}")

        # Scheduled
        sched = r["scheduled"]
        sched_icon = "✅" if sched else "❌"
        lines.append(f"     {sched_icon} Scheduled : {r['details'].get('scheduled', '?')}")

        # Firing
        fire = r["firing"]
        fire_icon = _FIRING_ICON.get(fire, "❓")
        lines.append(f"     {fire_icon} Firing    : {r['details'].get('firing', '?')}")

        # Content
        content = r["content"]
        if content is None:
            c_icon = "—"
        elif content:
            c_icon = "✅"
        else:
            c_icon = "❌"
        lines.append(f"     {c_icon} Content   : {r['details'].get('content', '?')}")

        # Snippets
        if show_content and r.get("snippets"):
            lines.append(f"     {'─'*50}")
            lines.append(f"     Content snippets (up to 3):")
            for i, snippet in enumerate(r["snippets"], 1):
                lines.append(f"       [{i}] {snippet}")

        lines.append("")

    # Summary
    passes = sum(1 for r in results if r["overall"] == "pass")
    warns = sum(1 for r in results if r["overall"] == "warn")
    fails = sum(1 for r in results if r["overall"] == "fail")
    lines.append(f"{'─'*70}")
    lines.append(f"  Summary: {passes} pass  |  {warns} warn  |  {fails} fail  (of {len(results)} agents)")

    if fails > 0 or warns > 0:
        lines.append("")
        lines.append("  Triage:")
        for r in results:
            if r["overall"] in ("fail", "warn"):
                agent = r["agent"]
                lines.append(f"    {agent}:")
                if not r["scheduled"]:
                    lines.append(f"      → Not scheduled. Fix: AGENT_NAME={agent} python3 scripts/nightly-nap/schedule_nap_cron.py --install")
                if r["firing"] in ("never", "stale"):
                    lines.append(f"      → Not firing recently. Check ~/.hermes/logs/nightly_nap.log on {agent}'s box.")
                    lines.append(f"        Manual run: AGENT_NAME={agent} python3 scripts/nightly-nap/nightly_nap.py --dry-run")
                if r["content"] is False:
                    lines.append(f"      → No Qdrant content. Either nap never ran or Qdrant collection is empty.")
                    lines.append(f"        Query: python3 scripts/nightly-nap/query_nap_summaries.py --agent {agent} --stats")

    lines.append(f"{'='*70}\n")
    return "\n".join(lines)


# ── Main ───────────────────────────────────────────────────────────────────

def parse_args(fleet_agents: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Audit per-agent short-term .md → holographic storage copy",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    default_agents = os.environ.get("AGENT_NAME", "").strip()
    p.add_argument(
        "--agents",
        nargs="+",
        default=([default_agents] if default_agents else fleet_agents),
        metavar="NAME",
        help=f"Agents to audit (default: live fleet from ACC registry)",
    )
    p.add_argument(
        "--stale-days",
        type=int,
        default=STALE_THRESHOLD_DAYS,
        metavar="N",
        help=f"Nap older than N days is considered stale (default: {STALE_THRESHOLD_DAYS})",
    )
    p.add_argument(
        "--lookback-days",
        type=int,
        default=CONTENT_LOOKBACK_DAYS,
        metavar="N",
        help=f"Days to look back in Qdrant for nap content (default: {CONTENT_LOOKBACK_DAYS})",
    )
    p.add_argument(
        "--json",
        dest="json_output",
        action="store_true",
        help="Emit machine-readable JSON to stdout",
    )
    p.add_argument(
        "--show-content",
        action="store_true",
        help="Show Qdrant content snippets in the report",
    )
    p.add_argument(
        "--local-only",
        action="store_true",
        help="Check only local state files (no Qdrant call)",
    )
    p.add_argument(
        "--verbose",
        action="store_true",
        help="Enable debug logging",
    )
    return p.parse_args()


def main() -> int:
    # Resolve the fleet agent list once before parsing args so the default
    # for --agents reflects the live registry, not a hardcoded constant.
    fleet_agents, agents_source = _FLEET_AGENTS, _FLEET_AGENTS_SOURCE

    args = parse_args(fleet_agents)
    if args.verbose:
        logging.getLogger().setLevel(logging.DEBUG)

    # Load credentials
    for env_path in ["~/.acc/.env", "~/.hermes/.env", "~/.ccc/.env"]:
        load_env_file(env_path)

    # Qdrant API key
    qdrant_api_key: str | None = None
    if not args.local_only:
        try:
            from qdrant_common import get_qdrant_api_key
            qdrant_api_key = get_qdrant_api_key()
        except Exception as exc:
            log.warning("Could not obtain Qdrant API key (%s) — content check will be skipped", exc)

    # Get crontab once (shared across all agents on this box)
    crontab_content = _get_crontab()

    results = []
    for agent_name in args.agents:
        agent_name = agent_name.strip().lower()
        if not agent_name:
            continue
        r = audit_agent(
            agent_name=agent_name,
            crontab_content=crontab_content,
            stale_days=args.stale_days,
            lookback_days=args.lookback_days,
            qdrant_api_key=qdrant_api_key,
            show_content=args.show_content,
            local_only=args.local_only,
        )
        results.append(r)

    if args.json_output:
        print(json.dumps(results, indent=2))
    else:
        print(format_report(results, args.show_content, agents_source))

    # Exit code: 0 = all pass, 1 = any warn/fail
    any_problem = any(r["overall"] in ("warn", "fail") for r in results)
    return 1 if any_problem else 0


if __name__ == "__main__":
    sys.exit(main())
