#!/usr/bin/env node
/**
 * post-with-dedup.mjs — Dedup-gated workqueue item POST
 *
 * Wraps an CCC API item POST with a pre-flight Milvus similarity check.
 * If a near-dup exists (cosine similarity > threshold), skips the POST.
 * If Milvus/ollama is unreachable, falls through and posts anyway.
 *
 * Usage:
 *   node post-with-dedup.mjs \
 *     --title "My new idea" \
 *     --description "Detailed description" \
 *     --assignee "natasha" \
 *     --priority "idea" \
 *     [--threshold 0.85]
 *
 * Or pipe JSON:
 *   echo '{"title":"...","description":"...","assignee":"natasha","priority":"idea"}' \
 *     | node post-with-dedup.mjs --stdin
 *
 * Environment:
 *   CCC_URL      default: http://100.89.199.14:8789
 *   RCC_TOKEN    default: <YOUR_RCC_TOKEN>
 */

import { parseArgs } from 'node:util';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));

const CCC_URL   = process.env.CCC_URL   || 'http://100.89.199.14:8789';
const RCC_TOKEN = process.env.RCC_TOKEN || '<YOUR_RCC_TOKEN>';
const DEFAULT_THRESHOLD = parseFloat(process.env.DEDUP_THRESHOLD || '0.85');

// ── Dedup check (inline, avoids subprocess) ───────────────────────────────────
const OLLAMA_BASE_URL = process.env.OLLAMA_BASE_URL || 'http://localhost:11434';
const MILVUS_ADDRESS  = process.env.MILVUS_ADDRESS  || '100.89.199.14:19530';

async function embedText(text) {
  const resp = await fetch(`${OLLAMA_BASE_URL}/api/embed`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: 'nomic-embed-text', input: text.slice(0, 2000) }),
    signal: AbortSignal.timeout(15_000),
  });
  if (!resp.ok) throw new Error(`ollama ${resp.status}`);
  return (await resp.json()).embeddings[0];
}

async function checkDup(title, description, threshold) {
  const text = `${title}\n${description}`.trim();
  const vector = await embedText(text);

  const { MilvusClient } = await import('@zilliz/milvus2-sdk-node');
  const client = new MilvusClient({ address: MILVUS_ADDRESS });

  const collections = await client.showCollections();
  if (!(collections.data || []).some(c => c.name === 'rcc_queue')) return null;

  await client.loadCollectionSync({ collection_name: 'rcc_queue' });
  const results = await client.search({
    collection_name: 'rcc_queue',
    vectors: [vector],
    vector_type: 'FloatVector',
    limit: 3,
    output_fields: ['id', 'title', 'status', 'priority'],
    metric_type: 'COSINE',
  });

  const active = (results.results || []).filter(r =>
    r.score >= threshold &&
    !['completed', 'cancelled', 'done', 'rejected'].includes(r.status)
  );
  return active.length > 0 ? active[0] : null;
}

// ── Post to CCC API ───────────────────────────────────────────────────────────
async function postItem(item) {
  const resp = await fetch(`${CCC_URL}/api/queue`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': `Bearer ${RCC_TOKEN}`,
    },
    body: JSON.stringify(item),
  });
  if (!resp.ok) throw new Error(`CCC POST failed: ${resp.status}`);
  return resp.json();
}

// ── Main ──────────────────────────────────────────────────────────────────────
async function main() {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      title:       { type: 'string' },
      description: { type: 'string', short: 'd' },
      assignee:    { type: 'string', default: 'natasha' },
      priority:    { type: 'string', default: 'idea' },
      threshold:   { type: 'string' },
      stdin:       { type: 'boolean' },
      dry_run:     { type: 'boolean' },
    },
  });

  let item;
  if (values.stdin) {
    item = JSON.parse(readFileSync('/dev/stdin', 'utf8'));
  } else {
    item = {
      title:       values.title || '',
      description: values.description || '',
      assignee:    values.assignee,
      priority:    values.priority,
    };
  }

  if (!item.title) { console.error('--title required'); process.exit(1); }

  const threshold = values.threshold ? parseFloat(values.threshold) : DEFAULT_THRESHOLD;

  // ── Pre-flight dedup ───────────────────────────────────────────────────────
  let dupResult = null;
  try {
    dupResult = await checkDup(item.title, item.description || '', threshold);
  } catch (err) {
    console.error(`[post-with-dedup] dedup check failed (${err.message}) — posting anyway`);
  }

  if (dupResult) {
    console.log(JSON.stringify({
      posted: false,
      reason: 'near_dup',
      near_dup: dupResult,
      threshold,
      proposed: { title: item.title },
    }));
    process.exit(0);
  }

  // ── Post ────────────────────────────────────────────────────────────────────
  if (values.dry_run) {
    console.log(JSON.stringify({ posted: false, reason: 'dry_run', would_post: item }));
    process.exit(0);
  }

  const result = await postItem(item);
  console.log(JSON.stringify({ posted: true, item: result }));
}

main().catch(err => {
  console.error('[post-with-dedup] fatal:', err.message);
  process.exit(1);
});
