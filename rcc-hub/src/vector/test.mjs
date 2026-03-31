/**
 * rcc/vector/test.mjs — Milvus integration smoke test
 *
 * Run: node rcc/vector/test.mjs
 * Requires: NVIDIA_API_KEY env var + Milvus on localhost:19530
 */

import {
  ensureCollections,
  vectorHealth,
  indexLesson,
  searchLessons,
  indexQueueItem,
  searchQueue,
  rememberSnippet,
  recallMemory,
} from './index.mjs';

const EMBED_API_KEY = process.env.NVIDIA_API_KEY || process.env.OPENAI_API_KEY;
if (!EMBED_API_KEY) {
  console.error('❌ Set NVIDIA_API_KEY env var to run this test');
  process.exit(1);
}

let passed = 0;
let failed = 0;

function ok(name, val) {
  if (val) {
    console.log(`  ✅ ${name}`);
    passed++;
  } else {
    console.error(`  ❌ ${name}`);
    failed++;
  }
}

async function run() {
  console.log('\n🐿️ Milvus Vector Search — Smoke Test\n');

  // 1. Health check
  console.log('── Health ──────────────────────────────────────');
  const health = await vectorHealth();
  console.log('   ', health);
  ok('Milvus is healthy', health.ok);

  // 2. Ensure collections
  console.log('\n── Collections ──────────────────────────────────');
  await ensureCollections();
  ok('ensureCollections() completed', true);

  // 3. Lesson index + search
  console.log('\n── Lessons ──────────────────────────────────────');
  const lesson = {
    id: `l-test-${Date.now()}`,
    agent: 'rocky',
    domain: 'express',
    tags: ['express5', 'wildcard', 'routing'],
    symptom: 'path-to-regexp throws an error on wildcard routes in Express 5',
    fix: 'Use /route/*splat syntax instead of /route/* in Express 5 — named wildcards only',
    score: 2,
    ts: new Date().toISOString(),
  };

  await indexLesson(lesson);
  ok('indexLesson() succeeded', true);

  const hits = await searchLessons('wildcard routing error express', 3);
  console.log(`   Found ${hits.length} lesson(s)`);
  ok('searchLessons() returned results', hits.length > 0);
  if (hits[0]) {
    ok('top hit matches lesson domain', hits[0].domain === 'express');
    console.log(`   Top hit (score=${hits[0].score?.toFixed(4)}): ${hits[0].symptom?.slice(0, 60)}`);
  }

  // 4. Queue index + search
  console.log('\n── Queue ────────────────────────────────────────');
  const item = {
    id: `wq-test-${Date.now()}`,
    title: 'Implement semantic search for lessons',
    description: 'Replace keyword matching in lessons module with Milvus vector search using NV-Embed-v2',
    status: 'pending',
    priority: 'high',
    tags: ['milvus', 'vector', 'search'],
    createdAt: new Date().toISOString(),
  };

  await indexQueueItem(item);
  ok('indexQueueItem() succeeded', true);

  const qhits = await searchQueue('add vector database for semantic retrieval', 3);
  console.log(`   Found ${qhits.length} queue item(s)`);
  ok('searchQueue() returned results', qhits.length > 0);
  if (qhits[0]) {
    console.log(`   Top hit (score=${qhits[0].score?.toFixed(4)}): ${qhits[0].title}`);
  }

  // 5. Memory snippet
  console.log('\n── Memory ───────────────────────────────────────');
  const content = 'The Milvus vector DB is running on localhost:19530. Use COSINE similarity for all collections. NV-Embed-v2 produces 4096-dim vectors.';
  const memId = await rememberSnippet(content, 'TOOLS.md', 'rocky');
  ok('rememberSnippet() returned id', typeof memId === 'string' && memId.length > 0);
  console.log(`   Stored memory id: ${memId}`);

  const recall = await recallMemory('where is Milvus running', 3, 'rocky');
  ok('recallMemory() returned results', recall.length > 0);
  if (recall[0]) {
    console.log(`   Recalled (score=${recall[0].score?.toFixed(4)}): ${recall[0].content?.slice(0, 60)}`);
  }

  // ── Summary ──────────────────────────────────────────────────────────────
  console.log(`\n${'─'.repeat(50)}`);
  console.log(`Results: ${passed} passed, ${failed} failed`);
  if (failed > 0) process.exit(1);
  else console.log('🐿️ All checks passed — Milvus integration is live!\n');
}

run().catch(err => {
  console.error('Fatal:', err);
  process.exit(1);
});
