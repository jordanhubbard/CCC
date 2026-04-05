#!/usr/bin/env node
/**
 * check-dedup.mjs — Pre-submission workqueue item dedup
 *
 * Checks whether a proposed new item is a near-duplicate of an existing
 * pending/incubating item using two strategies:
 *
 * Strategy A (default): Embedding similarity via ollama nomic-embed-text
 *   Embeds title+description, computes cosine similarity against all
 *   pending items fetched from CCC API. No Milvus dependency.
 *
 * Strategy B (--milvus): Query rcc_queue_sparky Milvus collection (768-dim)
 *   Faster for large queues, requires items to have been ingested first.
 *
 * Usage:
 *   node check-dedup.mjs --title "My idea" [--description "..."] [--threshold 0.85]
 *   echo '{"title":"..."}' | node check-dedup.mjs --stdin
 *
 * Exit codes:
 *   0 = no near-dup (safe to post)
 *   1 = near-dup found (skip)
 *   2 = error (allow post)
 */

import { parseArgs } from 'node:util';
import { readFileSync, appendFileSync } from 'node:fs';

const OLLAMA_BASE_URL  = process.env.OLLAMA_BASE_URL  || 'http://localhost:11434';
const OLLAMA_MODEL     = 'nomic-embed-text';
const CCC_URL          = process.env.CCC_URL          || 'http://100.89.199.14:8789';
const RCC_TOKEN        = process.env.RCC_TOKEN        || '<YOUR_RCC_TOKEN>';
const DEDUP_THRESHOLD  = parseFloat(process.env.DEDUP_THRESHOLD || '0.85');
const DEDUP_LOG_PATH   = process.env.DEDUP_LOG_PATH   || '/tmp/wq-dedup-skips.jsonl';

async function embed(text) {
  const resp = await fetch(`${OLLAMA_BASE_URL}/api/embed`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: OLLAMA_MODEL, input: text.slice(0, 2000) }),
    signal: AbortSignal.timeout(15_000),
  });
  if (!resp.ok) throw new Error(`ollama ${resp.status}`);
  return (await resp.json()).embeddings[0];
}

function cosineSim(a, b) {
  let dot = 0, na = 0, nb = 0;
  for (let i = 0; i < a.length; i++) { dot += a[i]*b[i]; na += a[i]*a[i]; nb += b[i]*b[i]; }
  return dot / (Math.sqrt(na) * Math.sqrt(nb) || 1);
}

async function fetchPendingItems() {
  const resp = await fetch(`${CCC_URL}/api/queue`, {
    headers: { 'Authorization': `Bearer ${RCC_TOKEN}` },
    signal: AbortSignal.timeout(10_000),
  });
  if (!resp.ok) throw new Error(`CCC API ${resp.status}`);
  const data = await resp.json();
  return (data.items || []).filter(i =>
    ['pending','in_progress','in-progress','incubating'].includes(i.status)
  );
}

async function main() {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      title:       { type: 'string' },
      description: { type: 'string', short: 'd', default: '' },
      threshold:   { type: 'string' },
      stdin:       { type: 'boolean' },
      quiet:       { type: 'boolean', short: 'q' },
    },
  });

  let title = values.title || '';
  let description = values.description || '';

  if (values.stdin) {
    const raw = JSON.parse(readFileSync('/dev/stdin', 'utf8'));
    title = raw.title || title;
    description = raw.description || description;
  }

  if (!title) { console.error('--title required'); process.exit(2); }

  const threshold = values.threshold ? parseFloat(values.threshold) : DEDUP_THRESHOLD;
  const text = `${title}\n${description}`.trim();

  // Embed the proposed item
  let propVec;
  try {
    propVec = await embed(text);
  } catch (err) {
    if (!values.quiet) console.error(`[dedup] embed failed: ${err.message} — allowing post`);
    console.log(JSON.stringify({ dup: false, reason: 'embed_error', error: err.message }));
    process.exit(2);
  }

  // Fetch all pending items from CCC and embed+compare each
  let candidates;
  try {
    candidates = await fetchPendingItems();
  } catch (err) {
    if (!values.quiet) console.error(`[dedup] CCC fetch failed: ${err.message} — allowing post`);
    console.log(JSON.stringify({ dup: false, reason: 'rcc_error', error: err.message }));
    process.exit(2);
  }

  // Embed each candidate and compute similarity
  // Batch: embed all titles concurrently (they're short)
  const results = [];
  await Promise.all(candidates.slice(0, 50).map(async item => {
    try {
      const candText = `${item.title}\n${item.description || ''}`.trim();
      const candVec = await embed(candText);
      const sim = cosineSim(propVec, candVec);
      if (sim >= threshold) {
        results.push({ id: item.id, title: item.title, status: item.status, priority: item.priority, score: sim });
      }
    } catch { /* skip errors on individual items */ }
  }));

  results.sort((a, b) => b.score - a.score);

  if (results.length === 0) {
    console.log(JSON.stringify({ dup: false, checked: Math.min(candidates.length, 50), threshold }));
    process.exit(0);
  }

  const best = results[0];
  try {
    appendFileSync(DEDUP_LOG_PATH, JSON.stringify({
      ts: new Date().toISOString(), proposed: { title, description: description.slice(0,200) },
      near_dup: best, threshold
    }) + '\n');
  } catch { /* non-fatal */ }

  if (!values.quiet) console.error(`[dedup] Near-dup (score=${best.score.toFixed(3)}): "${best.title}"`);
  console.log(JSON.stringify({ dup: true, score: best.score, threshold, near_dup: best }));
  process.exit(1);
}

main().catch(err => {
  console.error('[dedup] fatal:', err.message);
  console.log(JSON.stringify({ dup: false, reason: 'fatal', error: err.message }));
  process.exit(2);
});
