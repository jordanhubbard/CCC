#!/usr/bin/env python3
"""
Semantic search over Natasha's local embedding index.
Usage: python3 search-embeddings.py "your query here" [--top 5]
"""

import os, sys, json, argparse
import numpy as np
from pathlib import Path

HF_HOME = "/home/jkh/.openclaw/workspace/models/hf-cache"
os.environ["HF_HOME"] = HF_HOME

WORKSPACE = Path("/home/jkh/.openclaw/workspace")
INDEX_DIR = WORKSPACE / "models" / "embeddings"
INDEX_FILE = INDEX_DIR / "index.json"
VECTORS_FILE = INDEX_DIR / "vectors.npy"

def search(query, top_k=5):
    if not INDEX_FILE.exists():
        print("No index found. Run build-embedding-index.py first.")
        sys.exit(1)

    from sentence_transformers import SentenceTransformer
    import torch

    with open(INDEX_FILE) as f:
        index = json.load(f)

    vectors = np.load(VECTORS_FILE).astype(np.float32)
    chunks = index["chunks"]

    device = "cuda" if torch.cuda.is_available() else "cpu"
    model = SentenceTransformer(index["model"], trust_remote_code=True, device=device)

    q_vec = model.encode([f"search_query: {query}"], normalize_embeddings=True)[0]
    scores = vectors @ q_vec  # cosine similarity (vectors are normalized)
    top_idx = np.argsort(scores)[::-1][:top_k]

    print(f"\n🔍 Query: \"{query}\"\n")
    for rank, idx in enumerate(top_idx, 1):
        chunk = chunks[idx]
        score = scores[idx]
        print(f"[{rank}] {chunk['path']} (chunk {chunk['chunk_index']}/{chunk['total_chunks']-1}) — score: {score:.3f}")
        print(f"    {chunk['preview']}...")
        print()

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("query", help="Search query")
    parser.add_argument("--top", type=int, default=5, help="Number of results")
    args = parser.parse_args()
    search(args.query, args.top)
