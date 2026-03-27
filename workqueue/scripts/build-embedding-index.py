#!/usr/bin/env python3
"""
Natasha's local embedding index builder.
Indexes workspace files (memory, proposals, notes, etc.) using nomic-embed on GPU.
Output: workspace/models/embeddings/index.json + vectors.npy
"""

import os, sys, json, glob, hashlib
import numpy as np
from pathlib import Path
from datetime import datetime, timezone

HF_HOME = "/home/jkh/.openclaw/workspace/models/hf-cache"
os.environ["HF_HOME"] = HF_HOME

WORKSPACE = Path("/home/jkh/.openclaw/workspace")
INDEX_DIR = WORKSPACE / "models" / "embeddings"
INDEX_FILE = INDEX_DIR / "index.json"
VECTORS_FILE = INDEX_DIR / "vectors.npy"

# File patterns to index
INCLUDE_PATTERNS = [
    "*.md",
    "memory/*.md",
    "workqueue/proposals/*.md",
    "workqueue/*.md",
]
EXCLUDE_DIRS = {".git", "__pycache__", "models", "renders", "avatars"}

def collect_files():
    files = []
    for pattern in INCLUDE_PATTERNS:
        for f in WORKSPACE.glob(pattern):
            if not any(ex in f.parts for ex in EXCLUDE_DIRS):
                files.append(f)
    # Also grab any .txt files in memory/
    for f in (WORKSPACE / "memory").glob("*.txt"):
        files.append(f)
    return sorted(set(files))

def chunk_text(text, max_chars=1500, overlap=200):
    """Split text into overlapping chunks."""
    if len(text) <= max_chars:
        return [text]
    chunks = []
    start = 0
    while start < len(text):
        end = min(start + max_chars, len(text))
        chunks.append(text[start:end])
        if end == len(text):
            break
        start = end - overlap
    return chunks

def file_hash(path):
    return hashlib.md5(Path(path).read_bytes()).hexdigest()

def main():
    from sentence_transformers import SentenceTransformer
    import torch

    print(f"CUDA: {torch.cuda.is_available()} | GPU: {torch.cuda.get_device_name(0) if torch.cuda.is_available() else 'cpu'}")

    device = "cuda" if torch.cuda.is_available() else "cpu"
    print("Loading model...")
    model = SentenceTransformer("nomic-ai/nomic-embed-text-v1.5", trust_remote_code=True, device=device)
    print(f"Model loaded on {model.device}")

    INDEX_DIR.mkdir(parents=True, exist_ok=True)

    # Load existing index if present
    existing = {}
    existing_vectors = []
    if INDEX_FILE.exists() and VECTORS_FILE.exists():
        with open(INDEX_FILE) as f:
            old_index = json.load(f)
        existing = {item["path"]: item for item in old_index.get("chunks", [])}
        existing_vectors = list(np.load(VECTORS_FILE))
        print(f"Loaded existing index: {len(existing)} chunks")

    files = collect_files()
    print(f"Found {len(files)} files to index")

    chunks_meta = []
    texts_to_embed = []
    chunk_indices = []  # which chunks need new embeddings

    for fpath in files:
        try:
            text = fpath.read_text(encoding="utf-8", errors="ignore").strip()
        except Exception as e:
            print(f"  Skip {fpath.name}: {e}")
            continue
        if not text:
            continue

        fhash = file_hash(fpath)
        rel = str(fpath.relative_to(WORKSPACE))
        chunks = chunk_text(text)

        for i, chunk in enumerate(chunks):
            chunk_id = f"{rel}::{i}"
            meta = {
                "id": chunk_id,
                "path": rel,
                "chunk_index": i,
                "total_chunks": len(chunks),
                "file_hash": fhash,
                "char_count": len(chunk),
                "preview": chunk[:120].replace("\n", " "),
            }
            chunks_meta.append((meta, chunk))

    print(f"Total chunks: {len(chunks_meta)}")

    # Embed all chunks
    all_texts = [f"search_document: {c}" for _, c in chunks_meta]
    print(f"Embedding {len(all_texts)} chunks on {device}...")

    batch_size = 64
    all_vectors = []
    for i in range(0, len(all_texts), batch_size):
        batch = all_texts[i:i+batch_size]
        vecs = model.encode(batch, show_progress_bar=False, normalize_embeddings=True)
        all_vectors.append(vecs)
        print(f"  {min(i+batch_size, len(all_texts))}/{len(all_texts)}")

    vectors = np.vstack(all_vectors).astype(np.float32)

    # Save
    index_data = {
        "built_at": datetime.now(timezone.utc).isoformat(),
        "model": "nomic-ai/nomic-embed-text-v1.5",
        "device": str(device),
        "workspace": str(WORKSPACE),
        "chunk_count": len(chunks_meta),
        "file_count": len(files),
        "chunks": [m for m, _ in chunks_meta],
    }
    with open(INDEX_FILE, "w") as f:
        json.dump(index_data, f, indent=2)
    np.save(VECTORS_FILE, vectors)

    print(f"\n✅ Index built: {len(chunks_meta)} chunks from {len(files)} files")
    print(f"   Saved to {INDEX_DIR}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
