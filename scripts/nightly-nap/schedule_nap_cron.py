#!/usr/bin/env python3
"""
schedule_nap_cron.py — Install (or print) per-agent nightly nap cron entries.

Each agent calls this once to install its own crontab entry.  The script:
  1. Computes the agent's deterministic UTC offset (from nightly_nap.py logic).
  2. Checks that no two known agents collide within MIN_SPREAD_MINUTES; if
     they would, bumps the current agent's offset by MIN_SPREAD_MINUTES steps
     until the floor is respected.
  3. Prints the cron line (--print) or installs it into the user crontab.

Time reference: UTC midnight.  Every agent targets the same calendar-day
boundary regardless of host timezone.  The cron daemon on each box must be
running with UTC or a timezone that maps midnight to 00:00 UTC; the safest
approach is to export TZ=UTC in the crontab.

Usage:
    # Show what would be installed (no changes):
    python3 schedule_nap_cron.py --print

    # Install into current user's crontab:
    python3 schedule_nap_cron.py --install

    # Override agent name (default: $AGENT_NAME):
    python3 schedule_nap_cron.py --install --agent rocky

    # List currently installed nap cron lines:
    python3 schedule_nap_cron.py --list

    # Remove nap cron line:
    python3 schedule_nap_cron.py --remove

Known agents and their offsets are printed for the whole fleet so an operator
can verify no two agents land within MIN_SPREAD_MINUTES.

Fleet agent list resolution order (first that yields results wins):
  1. ACC registry API  — GET $ACC_URL/api/agents/names (requires ACC_AGENT_TOKEN)
  2. agents.json file  — $ACC_DATA_DIR/agents.json (or ~/.local/state/acc/agents.json)
  3. ACC_FLEET_AGENTS  — comma-separated env var, e.g. "rocky,natasha,bullwinkle,boris"
  4. Built-in default  — the four founding agents (last-resort / offline fallback)

If the resolved list differs from the built-in default a warning is printed so
operators know the source that was used.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

# ── Constants (must match nightly_nap.py) ─────────────────────────────────
NAP_WINDOW_MINUTES = 240   # 00:00–04:00 UTC
MIN_SPREAD_MINUTES = 15
CRON_TAG = "# acc-nightly-nap"

# Founding-fleet fallback — used only when all live sources are unavailable.
# Do NOT add new agents here; register them with the ACC server instead.
_BUILTIN_DEFAULT_AGENTS = ["rocky", "natasha", "bullwinkle", "boris"]

NAP_SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "nightly_nap.py")
LOG_FILE = os.path.expanduser("~/.hermes/logs/nightly_nap.log")


# ── Fleet agent resolution ─────────────────────────────────────────────────

def _load_env_file(path: str) -> None:
    """Load key=value pairs from an env file into os.environ (no-op if missing)."""
    p = Path(path).expanduser()
    if not p.exists():
        return
    with open(p) as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            k = k.strip()
            v = v.strip().strip('"').strip("'")
            if k and k not in os.environ:
                os.environ[k] = v


def _ensure_env_loaded() -> None:
    """Best-effort load of ~/.acc/.env so ACC_URL / ACC_AGENT_TOKEN are available."""
    for candidate in (
        os.path.expanduser("~/.acc/.env"),
        os.path.expanduser("~/.ccc/.env"),
    ):
        _load_env_file(candidate)


def _agents_from_api() -> list[str] | None:
    """
    Query GET $ACC_URL/api/agents/names and return the sorted agent name list.

    Returns None (not an empty list) on any error so callers can fall through
    to the next resolution strategy.
    """
    _ensure_env_loaded()

    acc_url = (
        os.environ.get("ACC_API_INTERNAL", "").strip()
        or os.environ.get("ACC_URL", "").strip()
    )
    token = (
        os.environ.get("ACC_AGENT_TOKEN", "").strip()
        or os.environ.get("ACC_AUTH_TOKEN", "").strip()
    )

    if not acc_url:
        return None

    url = acc_url.rstrip("/") + "/api/agents/names"
    req = urllib.request.Request(url)
    if token:
        req.add_header("Authorization", f"Bearer {token}")

    try:
        with urllib.request.urlopen(req, timeout=4) as resp:
            data = json.loads(resp.read().decode())
    except (urllib.error.URLError, OSError, json.JSONDecodeError):
        return None

    names = data.get("names")
    if not isinstance(names, list):
        return None

    cleaned = sorted(
        n.lower().strip()
        for n in names
        if isinstance(n, str) and n.strip()
    )
    return cleaned if cleaned else None


def _agents_from_file() -> list[str] | None:
    """
    Read agent names from the agents.json file used by acc-server.

    Path: $ACC_DATA_DIR/agents.json  (default ~/.local/state/acc/agents.json).
    Returns None on any error.
    """
    data_dir = (
        os.environ.get("ACC_DATA_DIR", "").strip()
        or os.path.expanduser("~/.local/state/acc")
    )
    agents_path = Path(data_dir) / "agents.json"

    if not agents_path.exists():
        return None

    try:
        with open(agents_path) as fh:
            data = json.load(fh)
    except (OSError, json.JSONDecodeError):
        return None

    if not isinstance(data, dict):
        return None

    cleaned = sorted(
        k.lower().strip()
        for k in data
        if isinstance(k, str) and k.strip()
        # exclude decommissioned agents
        and not (isinstance(data[k], dict) and data[k].get("onlineStatus") == "decommissioned")
    )
    return cleaned if cleaned else None


def _agents_from_env() -> list[str] | None:
    """
    Read agent names from the ACC_FLEET_AGENTS environment variable.

    Format: comma-separated names, e.g. "rocky,natasha,bullwinkle,boris,moose"
    Returns None if the variable is unset or empty.
    """
    raw = os.environ.get("ACC_FLEET_AGENTS", "").strip()
    if not raw:
        return None
    cleaned = sorted(
        n.lower().strip()
        for n in raw.split(",")
        if n.strip()
    )
    return cleaned if cleaned else None


def resolve_known_agents(verbose: bool = False) -> tuple[list[str], str]:
    """
    Return (agent_names, source_description) using the priority chain:
      1. ACC registry API
      2. agents.json file
      3. ACC_FLEET_AGENTS env var
      4. Built-in default

    'verbose' causes a warning to stderr when falling back past the API.
    """
    candidates: list[tuple[str, object]] = [
        ("ACC registry API",  _agents_from_api),
        ("agents.json file",  _agents_from_file),
        ("ACC_FLEET_AGENTS",  _agents_from_env),
        ("built-in default",  lambda: list(_BUILTIN_DEFAULT_AGENTS)),
    ]

    for source, loader in candidates:
        try:
            result = loader()
        except Exception:
            result = None

        if result:
            if verbose and source != "ACC registry API":
                print(
                    f"WARNING: fleet agent list loaded from {source}; "
                    "ACC registry API was unavailable or returned no agents.",
                    file=sys.stderr,
                )
            return result, source

    # Should never reach here (built-in default always succeeds), but be safe.
    return list(_BUILTIN_DEFAULT_AGENTS), "built-in default (emergency)"


# ── Offset computation ─────────────────────────────────────────────────────

def _raw_offset(agent_name: str) -> int:
    digest = hashlib.sha256(agent_name.lower().encode()).hexdigest()
    return int(digest[:8], 16) % NAP_WINDOW_MINUTES


def compute_offset(agent_name: str, known_agents: list[str]) -> int:
    """Return this agent's collision-adjusted offset in minutes after midnight."""
    my_raw = _raw_offset(agent_name)
    other_offsets = [
        _raw_offset(a) for a in known_agents if a.lower() != agent_name.lower()
    ]

    offset = my_raw
    max_iterations = NAP_WINDOW_MINUTES // MIN_SPREAD_MINUTES + 1
    for _ in range(max_iterations):
        conflicts = [
            o for o in other_offsets
            if abs((offset - o) % NAP_WINDOW_MINUTES) < MIN_SPREAD_MINUTES
            or abs((o - offset) % NAP_WINDOW_MINUTES) < MIN_SPREAD_MINUTES
        ]
        if not conflicts:
            break
        offset = (offset + MIN_SPREAD_MINUTES) % NAP_WINDOW_MINUTES

    return offset


def offset_to_cron(offset_minutes: int) -> tuple[str, str]:
    """Convert offset-after-midnight to cron minute + hour fields."""
    hour = offset_minutes // 60
    minute = offset_minutes % 60
    return str(minute), str(hour)


def build_cron_line(agent_name: str, offset_minutes: int) -> str:
    minute, hour = offset_to_cron(offset_minutes)
    return (
        f"{minute} {hour} * * * "
        f"TZ=UTC AGENT_NAME={agent_name} "
        f"{sys.executable} {NAP_SCRIPT} "
        f">> {LOG_FILE} 2>&1 "
        f"{CRON_TAG}"
    )


# ── Crontab helpers ────────────────────────────────────────────────────────

def get_current_crontab() -> str:
    try:
        result = subprocess.run(
            ["crontab", "-l"],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            return result.stdout
        # No crontab yet
        return ""
    except FileNotFoundError:
        print("ERROR: 'crontab' command not found — is cron installed?", file=sys.stderr)
        sys.exit(1)


def set_crontab(content: str) -> None:
    p = subprocess.run(
        ["crontab", "-"],
        input=content,
        capture_output=True, text=True,
    )
    if p.returncode != 0:
        print(f"ERROR installing crontab: {p.stderr}", file=sys.stderr)
        sys.exit(1)


# ── Fleet table ────────────────────────────────────────────────────────────

def fleet_table(known_agents: list[str], source: str) -> str:
    """Return a human-readable table of all known agents and their offsets."""
    lines = [
        f"{'Agent':<14} {'Offset (min)':<14} {'UTC time':<10}",
        "-" * 40,
    ]
    offsets = {a: compute_offset(a, known_agents) for a in known_agents}
    for agent, off in sorted(offsets.items(), key=lambda x: x[1]):
        minute, hour = offset_to_cron(off)
        lines.append(f"{agent:<14} {off:<14} {hour.zfill(2)}:{minute.zfill(2)} UTC")
    lines.append(f"\n  (fleet list source: {source})")
    return "\n".join(lines)


# ── Main ───────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Install nightly nap cron entry for an agent"
    )
    parser.add_argument("--agent", default=os.environ.get("AGENT_NAME", "").strip())
    parser.add_argument("--print", dest="just_print", action="store_true",
                        help="Print the cron line without installing")
    parser.add_argument("--install", action="store_true",
                        help="Install into current user crontab")
    parser.add_argument("--remove", action="store_true",
                        help="Remove nap cron line from current user crontab")
    parser.add_argument("--list", action="store_true",
                        help="List installed nap cron lines")
    parser.add_argument("--fleet", action="store_true",
                        help="Show all fleet agent offsets")
    args = parser.parse_args()

    # Resolve fleet agent list once; pass it everywhere so there is a single
    # source of truth for the current run.
    known_agents, agents_source = resolve_known_agents(verbose=True)

    if args.fleet:
        print(fleet_table(known_agents, agents_source))
        return

    if args.list:
        tab = get_current_crontab()
        nap_lines = [ln for ln in tab.splitlines() if CRON_TAG in ln]
        if nap_lines:
            print("\n".join(nap_lines))
        else:
            print("(no nap cron lines installed)")
        return

    if not args.agent:
        print("ERROR: --agent or AGENT_NAME required", file=sys.stderr)
        sys.exit(1)

    agent_name = args.agent
    offset = compute_offset(agent_name, known_agents)
    cron_line = build_cron_line(agent_name, offset)
    minute, hour = offset_to_cron(offset)

    if args.remove:
        tab = get_current_crontab()
        new_tab = "\n".join(
            ln for ln in tab.splitlines() if CRON_TAG not in ln
        ) + "\n"
        set_crontab(new_tab)
        print(f"Removed nap cron line for {agent_name}")
        return

    print(f"\nAgent      : {agent_name}")
    print(f"Nap offset : {offset} min after UTC midnight  ({hour.zfill(2)}:{minute.zfill(2)} UTC)")
    print(f"Cron line  : {cron_line}")
    print()
    print("Fleet schedule:")
    print(fleet_table(known_agents, agents_source))
    print()

    if args.just_print or (not args.install):
        print("(use --install to apply, --list to verify)")
        return

    # Install
    os.makedirs(os.path.dirname(LOG_FILE), exist_ok=True)

    tab = get_current_crontab()
    # Remove any existing nap line for this agent
    lines = [ln for ln in tab.splitlines() if CRON_TAG not in ln]
    lines.append(cron_line)
    new_tab = "\n".join(lines) + "\n"
    set_crontab(new_tab)
    print(f"✓ Nap cron installed for {agent_name} at {hour.zfill(2)}:{minute.zfill(2)} UTC daily")


if __name__ == "__main__":
    main()
