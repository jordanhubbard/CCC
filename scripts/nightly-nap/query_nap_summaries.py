#!/usr/bin/env python3
"""
query_nap_summaries.py — Query the Qdrant long-term store for nightly nap summaries.

Usage examples:
    # All summaries for a specific agent:
    python3 query_nap_summaries.py --agent rocky

    # Agent + specific date:
    python3 query_nap_summaries.py --agent rocky --date 2026-05-01

    # Semantic search within nap summaries:
    python3 query_nap_summaries.py --agent natasha --query "ideas about qdrant"

    # Show raw JSON (for piping):
    python3 query_nap_summaries.py --agent boris --date 2026-05-01 --json

    # Show collection stats:
    python3 query_nap_summaries.py --stats
"""

import argparse
import json
import os
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_QDRANT_PY = _HERE.parent / "qdrant-python"
if str(_QDRANT_PY) not in sys.path:
    sys.path.insert(0, str(_QDRANT_PY))

from qdrant_common import (  # noqa: E402
    get_qdrant_api_key,
    get_tokenhub_api_key,
    get_single_embedding,
    qdrant_get,
    qdrant_post,
)

COLLECTION_LT = "agent_long_term_memory"


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


def build_filter(agent: str | None, date: str | None) -> dict | None:
    must = [{"key": "chunk_type", "match": {"value": "nightly_nap"}}]
    if agent:
        must.append({"key": "agent", "match": {"value": agent}})
    if date:
        must.append({"key": "date", "match": {"value": date}})
    return {"must": must}


def scroll_points(api_key: str, filt: dict, limit: int = 50) -> list[dict]:
    """Scroll (non-vector) Qdrant points matching a filter."""
    body = {
        "filter": filt,
        "limit": limit,
        "with_payload": True,
        "with_vector": False,
    }
    try:
        result = qdrant_post(
            f"/collections/{COLLECTION_LT}/points/scroll",
            body,
            api_key=api_key,
        )
        return result.get("result", {}).get("points", [])
    except Exception as exc:
        print(f"ERROR: Qdrant scroll failed: {exc}", file=sys.stderr)
        return []


def semantic_search(
    api_key: str,
    tokenhub_key: str,
    query_text: str,
    filt: dict,
    limit: int = 5,
) -> list[dict]:
    """Vector search within nap summaries."""
    try:
        vector = get_single_embedding(query_text, tokenhub_key=tokenhub_key)
    except Exception as exc:
        print(f"ERROR: embedding failed: {exc}", file=sys.stderr)
        return []

    body = {
        "vector": vector,
        "limit": limit,
        "with_payload": True,
        "filter": filt,
    }
    try:
        result = qdrant_post(
            f"/collections/{COLLECTION_LT}/points/search",
            body,
            api_key=api_key,
        )
        return result.get("result", [])
    except Exception as exc:
        print(f"ERROR: Qdrant search failed: {exc}", file=sys.stderr)
        return []


def show_stats(api_key: str) -> None:
    try:
        info = qdrant_get(f"/collections/{COLLECTION_LT}", api_key=api_key)
        r = info["result"]
        pts = r["points_count"]
        status = r["status"]
        dims = r["config"]["params"]["vectors"]["size"]
        print(f"\n=== {COLLECTION_LT} ===")
        print(f"  Points : {pts}")
        print(f"  Dims   : {dims}")
        print(f"  Status : {status}")
        print()
    except Exception as exc:
        print(f"ERROR reading collection stats: {exc}", file=sys.stderr)


def print_points(points: list[dict], as_json: bool = False) -> None:
    if as_json:
        print(json.dumps(points, indent=2))
        return

    if not points:
        print("  (no results)")
        return

    for i, pt in enumerate(points):
        p = pt.get("payload", pt)  # scroll vs search have different shapes
        score = pt.get("score")
        print(f"\n--- Result {i + 1}" + (f"  score={score:.4f}" if score else "") + " ---")
        print(f"  Agent   : {p.get('agent', '?')}")
        print(f"  Date    : {p.get('date', '?')}")
        print(f"  Tags    : {p.get('tags', [])}")
        print(f"  Sessions: {p.get('session_ids', [])}")
        print(f"  Projects: {p.get('source_projects', [])}")
        print(f"  Tasks   : {p.get('source_tasks', [])}")
        print(f"  Chunk   : {p.get('chunk_index', '?')}/{p.get('total_chunks', '?')}")
        text = p.get("text", "")
        print(f"  ---")
        print(f"  {text[:600]}")
        if len(text) > 600:
            print(f"  … [{len(text)} chars total]")


def main() -> None:
    for env_path in ["~/.ccc/.env", "~/.hermes/.env", "/var/lib/tokenhub/env"]:
        load_env_file(env_path)

    parser = argparse.ArgumentParser(
        description="Query Qdrant long-term store for nightly nap summaries"
    )
    parser.add_argument("--agent", help="Filter by agent name")
    parser.add_argument("--date", metavar="YYYY-MM-DD", help="Filter by date")
    parser.add_argument("--query", help="Semantic search query")
    parser.add_argument("--limit", type=int, default=5, help="Max results (default 5)")
    parser.add_argument("--json", dest="as_json", action="store_true",
                        help="Output raw JSON")
    parser.add_argument("--stats", action="store_true",
                        help="Show collection stats and exit")
    args = parser.parse_args()

    try:
        api_key = get_qdrant_api_key()
    except Exception as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        sys.exit(1)

    if args.stats:
        show_stats(api_key)
        return

    filt = build_filter(args.agent, args.date)

    if not args.as_json:
        label_parts = []
        if args.agent:
            label_parts.append(f"agent={args.agent}")
        if args.date:
            label_parts.append(f"date={args.date}")
        if args.query:
            label_parts.append(f"query='{args.query}'")
        print(f"\n=== Nap summaries: {', '.join(label_parts) or 'all'} ===")

    if args.query:
        try:
            tokenhub_key = get_tokenhub_api_key()
        except Exception as exc:
            print(f"ERROR: {exc}", file=sys.stderr)
            sys.exit(1)
        results = semantic_search(api_key, tokenhub_key, args.query, filt, args.limit)
        print_points(results, as_json=args.as_json)
    else:
        points = scroll_points(api_key, filt, limit=args.limit)
        # scroll returns {id, payload}, massage to consistent shape
        display = [{"payload": p.get("payload", {})} for p in points]
        print_points(display, as_json=args.as_json)


if __name__ == "__main__":
    main()
