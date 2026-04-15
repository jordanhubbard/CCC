#!/usr/bin/env python3
"""
CCC Slack Channel Ingestion → Qdrant
Reads recent messages from #rockyandfriends (and optionally other channels),
chunks them, embeds via tokenhub, and upserts into Qdrant holographic memory.
Designed for cron — reads a watermark file to avoid re-ingesting.
"""

import json
import os
import sys
import time
import hashlib
import urllib.request
import urllib.error
from datetime import datetime, timezone

# ── Config ─────────────────────────────────────────────────────────
QDRANT_URL = os.environ.get("QDRANT_URL", "http://localhost:6333")
TOKENHUB_URL = os.environ.get("TOKENHUB_URL", "http://localhost:8090")
COLLECTION = "holographic_memory"
WATERMARK_DIR = os.path.expanduser("~/.ccc/watermarks")
EMBED_MODEL = "azure/openai/text-embedding-3-large"
EMBED_DIM = 3072

# Channels to ingest: name → channel_id
CHANNELS = {
    "rockyandfriends": "C0AMNRSN9EZ",
    "project-ccc": "C0ANY3AGW4Q",
    "project-tokenhub": "C0APF616SJZ",
}

MAX_MESSAGES_PER_CHANNEL = 50  # per run

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
load_env("/var/lib/tokenhub/env")

SLACK_TOKEN = os.environ.get("SLACK_OMGJKH_TOKEN", "")
TOKENHUB_API_KEY = os.environ.get("TOKENHUB_API_KEY", "")
QDRANT_API_KEY = ""
try:
    import subprocess
    r = subprocess.run(["docker", "inspect", "qdrant"], capture_output=True, text=True, timeout=10)
    for env in json.loads(r.stdout)[0]["Config"]["Env"]:
        if "API_KEY" in env:
            QDRANT_API_KEY = env.split("=", 1)[1]
except Exception:
    pass

os.makedirs(WATERMARK_DIR, exist_ok=True)

# ── HTTP helpers ───────────────────────────────────────────────────
def http_get(url, headers=None, timeout=10):
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read())
    except Exception as e:
        return {"ok": False, "error": str(e)}

def http_post(url, data, headers=None, timeout=15):
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    body = json.dumps(data).encode()
    req = urllib.request.Request(url, data=body, headers=hdrs, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return {"error": e.read().decode('utf-8', errors='replace')[:500]}
    except Exception as e:
        return {"error": str(e)}

def http_put(url, data, headers=None, timeout=15):
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    body = json.dumps(data).encode()
    req = urllib.request.Request(url, data=body, headers=hdrs, method="PUT")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return {"error": e.read().decode('utf-8', errors='replace')[:500]}
    except Exception as e:
        return {"error": str(e)}

# ── Watermark ──────────────────────────────────────────────────────
def get_watermark(channel_name):
    path = os.path.join(WATERMARK_DIR, f"{channel_name}.ts")
    if os.path.exists(path):
        return open(path).read().strip()
    return "0"

def set_watermark(channel_name, ts):
    path = os.path.join(WATERMARK_DIR, f"{channel_name}.ts")
    with open(path, 'w') as f:
        f.write(ts)

# ── Slack ──────────────────────────────────────────────────────────
def fetch_messages(channel_id, oldest="0"):
    url = f"https://slack.com/api/conversations.history?channel={channel_id}&oldest={oldest}&limit={MAX_MESSAGES_PER_CHANNEL}"
    data = http_get(url, headers={"Authorization": f"Bearer {SLACK_TOKEN}"})
    if not data.get("ok"):
        return []
    return data.get("messages", [])

def format_message(msg, channel_name):
    """Format a Slack message into a text chunk suitable for embedding."""
    user = msg.get("user", msg.get("username", "unknown"))
    ts = msg.get("ts", "0")
    text = msg.get("text", "")
    
    # Skip empty, bot join/leave, and very short messages
    if not text or len(text.strip()) < 10:
        return None
    
    dt = datetime.fromtimestamp(float(ts), tz=timezone.utc)
    date_str = dt.strftime("%Y-%m-%d %H:%M UTC")
    
    return {
        "id": hashlib.md5(f"{channel_name}:{ts}".encode()).hexdigest(),
        "text": f"[#{channel_name} {date_str}] {user}: {text}",
        "metadata": {
            "source": "slack",
            "channel": channel_name,
            "user": user,
            "timestamp": ts,
            "date": date_str,
            "type": "chat_message",
        }
    }

# ── Embedding ──────────────────────────────────────────────────────
def embed_texts(texts, batch_size=16):
    """Embed via tokenhub. Returns list of vectors."""
    all_vectors = []
    for i in range(0, len(texts), batch_size):
        batch = texts[i:i+batch_size]
        resp = http_post(f"{TOKENHUB_URL}/v1/embeddings", {
            "model": EMBED_MODEL,
            "input": batch,
        }, headers={"Authorization": f"Bearer {TOKENHUB_API_KEY}"}, timeout=30)
        if "error" in resp:
            print(f"  Embed error: {resp['error']}", file=sys.stderr)
            # Return zero vectors as fallback
            all_vectors.extend([[0.0]*EMBED_DIM] * len(batch))
            continue
        for item in resp.get("data", []):
            all_vectors.append(item["embedding"])
    return all_vectors

# ── Qdrant ─────────────────────────────────────────────────────────
def ensure_collection():
    """Create collection if it doesn't exist."""
    qdrant_headers = {}
    if QDRANT_API_KEY:
        qdrant_headers["api-key"] = QDRANT_API_KEY
    
    resp = http_get(f"{QDRANT_URL}/collections/{COLLECTION}", headers=qdrant_headers)
    if "result" in resp:
        return True  # exists
    
    # Create it
    resp = http_put(f"{QDRANT_URL}/collections/{COLLECTION}", {
        "vectors": {
            "size": EMBED_DIM,
            "distance": "Cosine",
        }
    }, headers=qdrant_headers)
    return "result" in resp

def upsert_points(points):
    """Upsert to Qdrant."""
    if not points:
        return 0
    qdrant_headers = {}
    if QDRANT_API_KEY:
        qdrant_headers["api-key"] = QDRANT_API_KEY
    
    resp = http_put(f"{QDRANT_URL}/collections/{COLLECTION}/points", {
        "points": points,
    }, headers=qdrant_headers)
    if "error" in resp:
        print(f"  Qdrant error: {resp['error']}", file=sys.stderr)
        return 0
    return len(points)

# ── Main ───────────────────────────────────────────────────────────
def main():
    if not SLACK_TOKEN:
        print(json.dumps({"error": "No SLACK_OMGJKH_TOKEN found"}))
        sys.exit(1)
    
    ensure_collection()
    
    report = {"channels": {}, "total_ingested": 0, "errors": []}
    
    for channel_name, channel_id in CHANNELS.items():
        watermark = get_watermark(channel_name)
        messages = fetch_messages(channel_id, oldest=watermark)
        
        if not messages:
            report["channels"][channel_name] = {"new": 0, "ingested": 0}
            continue
        
        # Format messages
        chunks = []
        for msg in messages:
            formatted = format_message(msg, channel_name)
            if formatted:
                chunks.append(formatted)
        
        if not chunks:
            report["channels"][channel_name] = {"new": len(messages), "ingested": 0, "note": "all filtered"}
            continue
        
        # Embed
        texts = [c["text"] for c in chunks]
        vectors = embed_texts(texts)
        
        if len(vectors) != len(chunks):
            report["errors"].append(f"{channel_name}: vector count mismatch")
            continue
        
        # Build Qdrant points
        points = []
        for chunk, vec in zip(chunks, vectors):
            # Use a deterministic numeric ID from the hash
            point_id = int(chunk["id"][:16], 16) & 0x7FFFFFFFFFFFFFFF
            points.append({
                "id": point_id,
                "vector": vec,
                "payload": chunk["metadata"] | {"text": chunk["text"]},
            })
        
        upserted = upsert_points(points)
        
        # Update watermark to latest message ts
        max_ts = max(msg.get("ts", "0") for msg in messages)
        set_watermark(channel_name, max_ts)
        
        report["channels"][channel_name] = {
            "new": len(messages),
            "ingested": upserted,
        }
        report["total_ingested"] += upserted
    
    print(json.dumps(report, indent=2))

if __name__ == "__main__":
    main()
