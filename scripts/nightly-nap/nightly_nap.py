#!/usr/bin/env python3
"""
nightly_nap.py — Nightly holographic → Qdrant summarize/copy for one agent.

Each agent runs this once per night at its own randomised offset after
midnight (hub time = UTC).  The offset is seeded from the agent name so
it is stable across restarts but different for every agent, guaranteeing
at least MIN_SPREAD_MINUTES separation between any two agents.

During the nap the agent:
  1. Sifts today's short-term holographic memories (daily notes, session
     messages, MEMORY.md snippets produced today) for ideas, wisdom,
     experience, inspiration, and project/task updates.
  2. Builds a structured summary.
  3. Writes that summary into the hub Qdrant collection
     "agent_long_term_memory" with full provenance metadata:
     agent, session ids, date, tags, source projects/tasks.

Scheduling contract
-------------------
  * Hub time = UTC.  Every agent uses UTC midnight as the reference so
    the fleet is synchronised on the same calendar day regardless of
    where each box is located.  Local wall-clock time is NOT used.
  * Each agent's offset is deterministic:
        offset_minutes = hash(agent_name) % NAP_WINDOW_MINUTES
    where NAP_WINDOW_MINUTES = 240 (00:00–04:00 UTC).
  * Minimum spread floor (MIN_SPREAD_MINUTES = 15) is enforced by the
    schedule_nap_cron.py companion script when it installs each agent's
    cron entry.  The hash-based offsets naturally spread the fleet, but
    the installer verifies and adjusts if two agents would land within
    the floor.

Usage
-----
  # Dry-run (no Qdrant writes, prints summary to stdout):
  python3 nightly_nap.py --dry-run

  # Normal run (writes to Qdrant):
  python3 nightly_nap.py

  # Run for a specific date (YYYY-MM-DD, defaults to today UTC):
  python3 nightly_nap.py --date 2026-05-01

Environment variables
---------------------
  AGENT_NAME            Agent identity (required; e.g. "rocky")
  QDRANT_URL            Qdrant base URL (default: http://localhost:6333)
  TOKENHUB_URL          Tokenhub base URL (default: http://localhost:8090)
  TOKENHUB_API_KEY      Tokenhub bearer key
  HERMES_HOME           Hermes home dir (default: ~/.hermes)
  NAP_DRY_RUN           Set to "1" to skip Qdrant writes (same as --dry-run)
  NAP_STATE_FILE        Override path for nap state JSON
                        (default: ~/.hermes/scripts/.nap_state.json)

Exit codes
----------
  0  success
  1  configuration / credential error
  2  already ran today (idempotence guard)
  3  summarisation or Qdrant error
"""

import argparse
import hashlib
import json
import logging
import os
import sqlite3
import sys
import time
import uuid
from datetime import date, datetime, timezone
from pathlib import Path

# ── stdlib-only import of shared Qdrant helpers ────────────────────────────
# scripts/qdrant-python/ is a sibling directory; add it to sys.path.
_HERE = Path(__file__).resolve().parent
_QDRANT_PY = _HERE.parent / "qdrant-python"
if str(_QDRANT_PY) not in sys.path:
    sys.path.insert(0, str(_QDRANT_PY))

from qdrant_common import (  # noqa: E402
    EMBEDDING_DIM,
    ensure_collection,
    get_embeddings,
    get_qdrant_api_key,
    get_tokenhub_api_key,
    qdrant_get,
    qdrant_post,
    qdrant_put,
    upsert_points,
    deterministic_point_id,
    chunk_text,
)

# ── Constants ──────────────────────────────────────────────────────────────

COLLECTION_LT = "agent_long_term_memory"   # hub long-term Qdrant collection
NAP_WINDOW_MINUTES = 240                   # 00:00–04:00 UTC nap window
MIN_SPREAD_MINUTES = 15                    # minimum inter-agent gap (floor)
MAX_SUMMARY_CHARS = 6000                   # cap fed to embedding
CHUNK_SIZE = 1400
CHUNK_OVERLAP = 180

logging.basicConfig(
    level=logging.INFO,
    format="[%(asctime)s] %(levelname)s %(message)s",
    datefmt="%Y-%m-%dT%H:%M:%SZ",
)
log = logging.getLogger("nightly_nap")


# ── Configuration helpers ──────────────────────────────────────────────────

def get_hermes_home() -> Path:
    val = os.environ.get("HERMES_HOME", "").strip()
    return Path(val) if val else Path.home() / ".hermes"


def agent_nap_offset(agent_name: str) -> int:
    """Return this agent's stable offset (minutes after UTC midnight).

    Computed as SHA-256(agent_name) mod NAP_WINDOW_MINUTES so it is
    deterministic, uniformly distributed, and at least 15 min apart from
    other agents when the fleet is small (verified by the installer).
    """
    digest = hashlib.sha256(agent_name.lower().encode()).hexdigest()
    return int(digest[:8], 16) % NAP_WINDOW_MINUTES


def load_env_file(path: str) -> None:
    """Load key=value pairs from an env file into os.environ (no-op if missing)."""
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


# ── Nap state (idempotence) ────────────────────────────────────────────────

def _state_file_path(agent_name: str) -> Path:
    override = os.environ.get("NAP_STATE_FILE", "").strip()
    if override:
        return Path(override).expanduser()
    hermes_home = get_hermes_home()
    scripts_dir = hermes_home / "scripts"
    scripts_dir.mkdir(parents=True, exist_ok=True)
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


def save_nap_state(agent_name: str, state: dict) -> None:
    path = _state_file_path(agent_name)
    state["updated_at"] = datetime.now(timezone.utc).isoformat()
    with open(path, "w") as f:
        json.dump(state, f, indent=2)


def already_ran_today(agent_name: str, target_date: date) -> bool:
    """Return True if we successfully completed a nap for target_date already."""
    state = load_nap_state(agent_name)
    last_date = state.get("last_successful_date", "")
    return last_date == target_date.isoformat()


# ── Short-term holographic memory sources ──────────────────────────────────

def collect_daily_notes(hermes_home: Path, target_date: date) -> list[dict]:
    """Read today's (and yesterday's) daily note files from memory/."""
    notes = []
    memory_dir = hermes_home / "memory"
    if not memory_dir.is_dir():
        return notes

    from datetime import timedelta
    candidates = [target_date, target_date - timedelta(days=1)]
    for d in candidates:
        note_file = memory_dir / f"{d.isoformat()}.md"
        if note_file.exists():
            text = note_file.read_text(encoding="utf-8", errors="replace").strip()
            if text:
                notes.append({
                    "source_type": "daily_note",
                    "date": d.isoformat(),
                    "path": str(note_file),
                    "text": text,
                })
    return notes


def collect_session_messages(hermes_home: Path, target_date: date, agent_name: str) -> tuple[list[dict], list[str]]:
    """Pull today's session messages from ~/.hermes/state.db.

    Returns (message_chunks, session_ids).
    """
    db_path = hermes_home / "state.db"
    if not db_path.exists():
        return [], []

    # epoch range for target_date UTC
    import calendar
    day_start = calendar.timegm(target_date.timetuple())  # 00:00:00 UTC
    day_end = day_start + 86400                           # 24:00:00 UTC

    chunks = []
    session_ids = []

    try:
        conn = sqlite3.connect(str(db_path))
        conn.row_factory = sqlite3.Row
        cur = conn.cursor()

        # Sessions started today
        cur.execute(
            "SELECT id, source, model, title, started_at FROM sessions "
            "WHERE started_at >= ? AND started_at < ? ORDER BY started_at",
            (day_start, day_end),
        )
        sessions = [dict(r) for r in cur.fetchall()]

        for sess in sessions:
            sid = sess["id"]
            session_ids.append(sid)

            cur.execute(
                "SELECT role, content, tool_name FROM messages "
                "WHERE session_id = ? ORDER BY rowid",
                (sid,),
            )
            rows = cur.fetchall()
            parts = []
            for row in rows:
                role = row["role"]
                content = (row["content"] or "").strip()
                if not content:
                    continue
                # Skip very long raw tool outputs
                if role == "tool" and len(content) > 1500:
                    content = content[:500] + f"\n… [{len(content)} chars]"
                label = {
                    "user": "User",
                    "assistant": f"Assistant ({agent_name})",
                    "system": "System",
                    "tool": f"Tool ({row['tool_name'] or '?'})",
                }.get(role, role)
                parts.append(f"{label}: {content}")

            if parts:
                started = datetime.fromtimestamp(
                    sess["started_at"], tz=timezone.utc
                ).strftime("%Y-%m-%d %H:%M UTC")
                header = (
                    f"Session {sid} | source={sess['source']} | "
                    f"model={sess['model']} | started={started}"
                )
                if sess.get("title"):
                    header += f" | title={sess['title']}"
                chunks.append({
                    "source_type": "session",
                    "session_id": sid,
                    "started_at": started,
                    "source": sess["source"],
                    "text": header + "\n\n" + "\n\n".join(parts),
                })

        conn.close()
    except Exception as exc:
        log.warning("Could not read state.db: %s", exc)

    return chunks, session_ids


def collect_memory_md(hermes_home: Path) -> list[dict]:
    """Return snippets from MEMORY.md (long-term curated memory)."""
    mem_file = hermes_home / "MEMORY.md"
    if not mem_file.exists():
        return []
    text = mem_file.read_text(encoding="utf-8", errors="replace").strip()
    if not text:
        return []
    return [{"source_type": "memory_md", "path": str(mem_file), "text": text}]


# ── Summary builder ────────────────────────────────────────────────────────

_SIFT_CATEGORIES = ["ideas", "wisdom", "experience", "inspiration", "project_task_updates"]

def build_summary(
    agent_name: str,
    target_date: date,
    daily_notes: list[dict],
    session_chunks: list[dict],
    memory_snippets: list[dict],
) -> dict:
    """Build a structured summary dict from raw holographic sources.

    The summary text is assembled from all sources, capped at
    MAX_SUMMARY_CHARS, and tagged with inferred categories.
    """
    all_texts = []

    # Daily notes first (most recent context)
    for item in daily_notes:
        all_texts.append(f"=== Daily Note ({item['date']}) ===\n{item['text']}")

    # Session conversations
    for item in session_chunks:
        all_texts.append(f"=== Session ({item.get('started_at', '?')}) ===\n{item['text']}")

    # MEMORY.md snippets (background wisdom)
    for item in memory_snippets:
        all_texts.append(f"=== MEMORY.md ===\n{item['text']}")

    combined = "\n\n---\n\n".join(all_texts)

    # Cap length
    if len(combined) > MAX_SUMMARY_CHARS:
        combined = combined[:MAX_SUMMARY_CHARS] + "\n\n… [truncated for embedding]"

    # Infer tags from content (lightweight keyword scan)
    tags = _infer_tags(combined)

    # Collect source projects and tasks mentioned
    source_projects = _extract_mentions(combined, prefix="project")
    source_tasks = _extract_mentions(combined, prefix="task")

    # Derive session ids
    session_ids = [s["session_id"] for s in session_chunks if "session_id" in s]

    return {
        "agent": agent_name,
        "date": target_date.isoformat(),
        "summary_text": combined,
        "session_ids": session_ids,
        "source_count": {
            "daily_notes": len(daily_notes),
            "sessions": len(session_chunks),
            "memory_snippets": len(memory_snippets),
        },
        "tags": tags,
        "source_projects": source_projects,
        "source_tasks": source_tasks,
        "sift_categories": _SIFT_CATEGORIES,
        "created_at": datetime.now(timezone.utc).isoformat(),
    }


def _infer_tags(text: str) -> list[str]:
    """Lightweight keyword → tag inference (no LLM required)."""
    tags: list[str] = []
    tl = text.lower()

    mapping = {
        "idea": ["idea", "proposal", "suggestion", "brainstorm"],
        "wisdom": ["lesson", "learned", "insight", "wisdom", "principle", "rule"],
        "experience": ["completed", "shipped", "deployed", "fixed", "resolved"],
        "inspiration": ["inspired", "exciting", "breakthrough", "elegant"],
        "project_update": ["project", "milestone", "phase", "sprint"],
        "task_update": ["task", "ticket", "issue", "pr", "pull request"],
        "bug": ["bug", "error", "crash", "fail", "exception"],
        "design": ["design", "architecture", "schema", "api"],
        "infra": ["docker", "kubernetes", "qdrant", "postgres", "redis", "server"],
    }
    for tag, keywords in mapping.items():
        if any(kw in tl for kw in keywords):
            tags.append(tag)

    return sorted(set(tags))


def _extract_mentions(text: str, prefix: str) -> list[str]:
    """Extract simple 'project: X' or 'task: X' mentions from text (up to 10)."""
    import re
    pattern = rf"{prefix}[:\s]+([A-Za-z0-9_\-]{{2,40}})"
    matches = re.findall(pattern, text, re.IGNORECASE)
    # Deduplicate, preserve order
    seen: list[str] = []
    for m in matches:
        if m.lower() not in [s.lower() for s in seen]:
            seen.append(m)
        if len(seen) >= 10:
            break
    return seen


# ── Qdrant write ───────────────────────────────────────────────────────────

def ensure_lt_collection(api_key: str) -> int:
    """Ensure the long-term memory collection exists with proper indexes."""
    count = ensure_collection(COLLECTION_LT, dim=EMBEDDING_DIM, api_key=api_key)

    # Add long-term-specific payload indexes
    for field, schema in [
        ("agent", "keyword"),
        ("date", "keyword"),
        ("chunk_type", "keyword"),
        ("tags", "keyword"),
    ]:
        try:
            qdrant_put(
                f"/collections/{COLLECTION_LT}/index",
                {"field_name": field, "field_schema": schema},
                api_key=api_key,
            )
        except Exception:
            pass  # index may already exist

    return count


def write_to_qdrant(
    summary: dict,
    api_key: str,
    tokenhub_key: str,
    dry_run: bool = False,
) -> int:
    """Chunk the summary text, embed, and upsert into Qdrant.

    Returns the number of points written (0 on dry-run).
    """
    text = summary["summary_text"]
    chunks = chunk_text(text, max_chars=CHUNK_SIZE, overlap=CHUNK_OVERLAP)
    if not chunks:
        log.warning("No chunks produced from summary — nothing to write")
        return 0

    total_chunks = len(chunks)
    agent = summary["agent"]
    date_str = summary["date"]

    log.info("Embedding %d chunks for %s / %s …", total_chunks, agent, date_str)

    if not dry_run:
        embeddings = get_embeddings(chunks, tokenhub_key=tokenhub_key)
    else:
        embeddings = [[0.0] * EMBEDDING_DIM for _ in chunks]

    points = []
    for i, (chunk, embedding) in enumerate(zip(chunks, embeddings)):
        point_id = deterministic_point_id(
            "nightly_nap", agent, date_str, i
        )
        payload = {
            "chunk_type": "nightly_nap",
            "agent": agent,
            "date": date_str,
            "date_range": date_str,            # single-day summary
            "session_ids": summary["session_ids"],
            "tags": summary["tags"],
            "sift_categories": summary["sift_categories"],
            "source_projects": summary["source_projects"],
            "source_tasks": summary["source_tasks"],
            "source_count": summary["source_count"],
            "chunk_index": i,
            "total_chunks": total_chunks,
            "created_at": summary["created_at"],
            "text": chunk,
        }
        points.append({"id": point_id, "vector": embedding, "payload": payload})

    if dry_run:
        log.info("[DRY-RUN] Would upsert %d points to %s", len(points), COLLECTION_LT)
        return 0

    upserted = upsert_points(COLLECTION_LT, points, api_key=api_key)
    log.info("Upserted %d points to %s", upserted, COLLECTION_LT)
    return upserted


# ── Status file (unreachable flag for fleet / dispatch) ────────────────────

def _status_file(agent_name: str) -> Path:
    return get_hermes_home() / f".nap_active_{agent_name}"


def mark_nap_start(agent_name: str) -> None:
    """Write a marker file so dispatch/fleet know the agent is napping."""
    sf = _status_file(agent_name)
    sf.write_text(
        json.dumps({
            "agent": agent_name,
            "nap_started_at": datetime.now(timezone.utc).isoformat(),
            "pid": os.getpid(),
        })
    )
    log.info("Nap marker written: %s", sf)


def mark_nap_end(agent_name: str) -> None:
    """Remove the nap marker file."""
    sf = _status_file(agent_name)
    try:
        sf.unlink()
        log.info("Nap marker removed: %s", sf)
    except FileNotFoundError:
        pass


def is_napping(agent_name: str) -> bool:
    """Return True if a nap marker file exists for this agent."""
    return _status_file(agent_name).exists()


# ── Main ───────────────────────────────────────────────────────────────────

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Nightly holographic → Qdrant nap for one agent",
    )
    p.add_argument(
        "--date",
        metavar="YYYY-MM-DD",
        help="Target date (defaults to today UTC)",
    )
    p.add_argument(
        "--dry-run",
        action="store_true",
        default=os.environ.get("NAP_DRY_RUN", "") == "1",
        help="Print summary without writing to Qdrant",
    )
    p.add_argument(
        "--force",
        action="store_true",
        help="Ignore the idempotence guard and run even if already ran today",
    )
    p.add_argument(
        "--agent",
        metavar="NAME",
        default=os.environ.get("AGENT_NAME", "").strip(),
        help="Agent name (overrides AGENT_NAME env var)",
    )
    return p.parse_args()


def main() -> int:
    args = parse_args()

    # Load .env files for credentials
    for env_path in [
        "~/.ccc/.env",
        "~/.hermes/.env",
        "/var/lib/tokenhub/env",
    ]:
        load_env_file(env_path)

    agent_name = args.agent
    if not agent_name:
        log.error(
            "AGENT_NAME not set — export AGENT_NAME=<name> or pass --agent <name>"
        )
        return 1

    # Resolve target date
    if args.date:
        try:
            target_date = date.fromisoformat(args.date)
        except ValueError:
            log.error("Invalid --date value '%s' (expected YYYY-MM-DD)", args.date)
            return 1
    else:
        target_date = datetime.now(timezone.utc).date()

    log.info(
        "=== Nightly Nap: agent=%s  date=%s  dry_run=%s ===",
        agent_name, target_date.isoformat(), args.dry_run,
    )

    # Idempotence guard
    if not args.force and already_ran_today(agent_name, target_date):
        log.info(
            "Already completed nap for %s on %s — skipping (use --force to override)",
            agent_name, target_date.isoformat(),
        )
        return 2

    # Credentials (skip on dry-run so it works without Qdrant)
    api_key = None
    tokenhub_key = None
    if not args.dry_run:
        try:
            api_key = get_qdrant_api_key()
        except Exception as exc:
            log.error("Could not obtain Qdrant API key: %s", exc)
            return 1
        try:
            tokenhub_key = get_tokenhub_api_key()
        except Exception as exc:
            log.error("Could not obtain Tokenhub API key: %s", exc)
            return 1

    hermes_home = get_hermes_home()

    # ── Mark nap start (agent becomes unreachable for fleet work) ──────────
    mark_nap_start(agent_name)

    try:
        # ── 1. Sift short-term holographic sources ─────────────────────────
        log.info("Sifting short-term holographic memories for %s / %s …",
                 agent_name, target_date)

        daily_notes = collect_daily_notes(hermes_home, target_date)
        log.info("  daily notes: %d file(s)", len(daily_notes))

        session_chunks, session_ids = collect_session_messages(
            hermes_home, target_date, agent_name
        )
        log.info("  session chunks: %d  (session_ids: %s)",
                 len(session_chunks), session_ids)

        memory_snippets = collect_memory_md(hermes_home)
        log.info("  MEMORY.md snippets: %d", len(memory_snippets))

        if not daily_notes and not session_chunks and not memory_snippets:
            log.info("No holographic sources found for %s — nothing to summarise",
                     target_date)
            # Still mark as ran so we don't retry all night
            state = load_nap_state(agent_name)
            state["last_successful_date"] = target_date.isoformat()
            state["last_run_result"] = "no_sources"
            save_nap_state(agent_name, state)
            return 0

        # ── 2. Summarise the sift ──────────────────────────────────────────
        log.info("Building summary …")
        summary = build_summary(
            agent_name=agent_name,
            target_date=target_date,
            daily_notes=daily_notes,
            session_chunks=session_chunks,
            memory_snippets=memory_snippets,
        )
        log.info(
            "Summary: %d chars, %d session(s), tags=%s",
            len(summary["summary_text"]),
            len(summary["session_ids"]),
            summary["tags"],
        )

        if args.dry_run:
            print("\n" + "=" * 70)
            print(f"[DRY-RUN] Nightly nap summary for {agent_name} / {target_date}")
            print("=" * 70)
            print(f"Tags          : {summary['tags']}")
            print(f"Sessions      : {summary['session_ids']}")
            print(f"Src projects  : {summary['source_projects']}")
            print(f"Src tasks     : {summary['source_tasks']}")
            print(f"Source counts : {summary['source_count']}")
            print("-" * 70)
            print(summary["summary_text"][:3000])
            if len(summary["summary_text"]) > 3000:
                print(f"\n… [{len(summary['summary_text'])} chars total, truncated for display]")
            print("=" * 70 + "\n")
            return 0

        # ── 3. Ensure collection and write to Qdrant ───────────────────────
        log.info("Ensuring Qdrant collection '%s' …", COLLECTION_LT)
        ensure_lt_collection(api_key)

        points_written = write_to_qdrant(
            summary=summary,
            api_key=api_key,
            tokenhub_key=tokenhub_key,
            dry_run=False,
        )

        if points_written == 0:
            log.error("No points were written to Qdrant")
            return 3

        # ── 4. Persist success state ───────────────────────────────────────
        state = load_nap_state(agent_name)
        state["last_successful_date"] = target_date.isoformat()
        state["last_run_result"] = "ok"
        state["last_points_written"] = points_written
        state["last_session_ids"] = session_ids
        save_nap_state(agent_name, state)

        # Collection stats
        try:
            info = qdrant_get(f"/collections/{COLLECTION_LT}", api_key=api_key)
            total_pts = info["result"]["points_count"]
            log.info(
                "=== Done: %d new points | %s total in %s ===",
                points_written, total_pts, COLLECTION_LT,
            )
        except Exception:
            log.info("=== Done: %d new points ===", points_written)

        return 0

    except Exception as exc:
        log.exception("Nightly nap failed: %s", exc)
        return 3

    finally:
        mark_nap_end(agent_name)


if __name__ == "__main__":
    sys.exit(main())
