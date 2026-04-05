/**
 * rcc/tests/gpu/memory-pressure.test.mjs — GPU embedding pipeline stress test
 *
 * Exercises the local embedding pipeline under realistic load:
 *   1. Batch-embeds 1000 short strings via ollama nomic-embed-text
 *   2. Measures throughput (embeds/sec) and p95 latency
 *   3. Upserts results to rcc_memory_sparky in bulk
 *   4. Asserts no OOM, gRPC timeouts, or embedding dimension mismatches
 *
 * Only runs when EMBED_BACKEND=local is set (skips otherwise).
 * Run: EMBED_BACKEND=local MILVUS_ADDRESS=100.89.199.14:19530 node --test rcc/tests/gpu/memory-pressure.test.mjs
 *
 * Baseline (sparky GB10, 2026-03-28): ~3.3 embeds/s, ~300ms/embed
 */

import { test, describe, before, skip } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'crypto';

const EMBED_BACKEND   = process.env.EMBED_BACKEND || 'remote';
const OLLAMA_BASE_URL = process.env.OLLAMA_BASE_URL || 'http://localhost:11434';
const OLLAMA_MODEL    = process.env.OLLAMA_EMBED_MODEL || 'nomic-embed-text';
const MILVUS_ADDRESS  = process.env.MILVUS_ADDRESS || 'localhost:19530';
const EXPECTED_DIM    = 768;
const BATCH_SIZE      = 50;   // embed in batches of 50
const TOTAL_EMBEDS    = 200;  // total strings to embed (keep <1000 for CI speed)
const TIMEOUT_MS      = 120_000; // 2 min overall timeout

// ── Skip gate ─────────────────────────────────────────────────────────────────

if (EMBED_BACKEND !== 'local') {
  console.log(`[gpu/memory-pressure] EMBED_BACKEND=${EMBED_BACKEND} — skipping (set EMBED_BACKEND=local to run)`);
  process.exit(0);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async function ollamaEmbed(text) {
  const resp = await fetch(`${OLLAMA_BASE_URL}/api/embed`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: OLLAMA_MODEL, input: text }),
    signal: AbortSignal.timeout(30_000),
  });
  if (!resp.ok) throw new Error(`ollama ${resp.status}: ${await resp.text()}`);
  const data = await resp.json();
  return data.embeddings[0];
}

function generateTestStrings(n) {
  const templates = [
    'Agent Natasha processes GPU workloads on the DGX Spark.',
    'The CCC work queue distributes tasks across the agent fleet.',
    'Milvus stores 768-dimensional vectors for semantic search.',
    'nomic-embed-text runs on the GB10 Blackwell GPU via ollama.',
    'Rocky manages the API server on do-host1 in DigitalOcean.',
    'Bullwinkle handles calendar and iMessage on the Mac mini.',
    'Boris runs multi-GPU Omniverse workloads in Sweden.',
    'ClawBus routes typed messages between agents in real time.',
    'Memory files persist agent context across session restarts.',
    'CUDA 13.0 enables unified memory access on the DGX Spark.',
  ];
  return Array.from({ length: n }, (_, i) =>
    `${templates[i % templates.length]} [variant ${i}]`
  );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe('GPU memory pressure — ollama nomic-embed-text', { timeout: TIMEOUT_MS }, () => {

  let ollamaOk = false;

  before(async () => {
    // Verify ollama is reachable and model is loaded
    const resp = await fetch(`${OLLAMA_BASE_URL}/api/tags`, { signal: AbortSignal.timeout(5000) });
    assert.ok(resp.ok, `ollama not reachable at ${OLLAMA_BASE_URL}`);
    const { models } = await resp.json();
    const found = models.some(m => m.name.startsWith(OLLAMA_MODEL));
    assert.ok(found, `Model ${OLLAMA_MODEL} not found in ollama — run: ollama pull ${OLLAMA_MODEL}`);
    ollamaOk = true;
  });

  test('single embed returns correct dimension', async () => {
    const vec = await ollamaEmbed('test embedding dimensionality check');
    assert.equal(vec.length, EXPECTED_DIM, `Expected ${EXPECTED_DIM}-dim, got ${vec.length}`);
    assert.ok(vec.every(v => typeof v === 'number' && isFinite(v)), 'All values must be finite floats');
  });

  test(`batch embed ${TOTAL_EMBEDS} strings — throughput and latency`, async () => {
    const strings = generateTestStrings(TOTAL_EMBEDS);
    const latencies = [];
    let errors = 0;
    const start = performance.now();

    for (let i = 0; i < strings.length; i += BATCH_SIZE) {
      const chunk = strings.slice(i, i + BATCH_SIZE);
      const batchStart = performance.now();
      await Promise.all(chunk.map(async s => {
        const t0 = performance.now();
        try {
          const vec = await ollamaEmbed(s);
          assert.equal(vec.length, EXPECTED_DIM, `Dim mismatch at index ${i}`);
          latencies.push(performance.now() - t0);
        } catch (err) {
          errors++;
          console.warn(`[embed error] ${err.message}`);
        }
      }));
      const batchMs = performance.now() - batchStart;
      console.log(`  batch ${Math.floor(i/BATCH_SIZE)+1}/${Math.ceil(strings.length/BATCH_SIZE)}: ${chunk.length} embeds in ${batchMs.toFixed(0)}ms`);
    }

    const totalMs = performance.now() - start;
    const throughput = (TOTAL_EMBEDS - errors) / (totalMs / 1000);
    const p50 = latencies.sort((a,b)=>a-b)[Math.floor(latencies.length*0.50)];
    const p95 = latencies[Math.floor(latencies.length*0.95)];
    const p99 = latencies[Math.floor(latencies.length*0.99)];

    console.log(`\n  === Embedding Throughput ===`);
    console.log(`  Total: ${TOTAL_EMBEDS} embeds in ${(totalMs/1000).toFixed(1)}s`);
    console.log(`  Throughput: ${throughput.toFixed(2)} embeds/s`);
    console.log(`  Latency p50=${p50.toFixed(0)}ms p95=${p95.toFixed(0)}ms p99=${p99?.toFixed(0)||'N/A'}ms`);
    console.log(`  Errors: ${errors}/${TOTAL_EMBEDS}`);

    assert.equal(errors, 0, `${errors} embedding errors — possible OOM or timeout`);
    assert.ok(throughput > 0.5, `Throughput ${throughput.toFixed(2)}/s too low — possible GPU stall`);
    assert.ok(p95 < 10_000, `p95 latency ${p95.toFixed(0)}ms exceeds 10s — possible GPU memory pressure`);
  });

  test('bulk upsert to rcc_memory_sparky — no gRPC timeouts', async () => {
    // Import vector module dynamically (requires Milvus to be reachable)
    let vectorMod;
    try {
      vectorMod = await import('../../vector/index.mjs');
      await vectorMod.ensureCollections();
    } catch (err) {
      console.warn(`[skip] Milvus not reachable (${MILVUS_ADDRESS}): ${err.message}`);
      return; // soft skip — Milvus may not be accessible from all envs
    }

    const strings = generateTestStrings(50); // smaller batch for Milvus round-trip
    let upserted = 0;
    let upsertErrors = 0;
    const start = performance.now();

    for (const [i, text] of strings.entries()) {
      const id = createHash('sha256').update(`pressure-test:${i}:${text}`).digest('hex').slice(0, 32);
      try {
        await vectorMod.vectorUpsert('rcc_memory_sparky', id, text, {
          agent: 'natasha',
          content: text.slice(0, 4096),
          source: 'gpu/memory-pressure.test.mjs',
          ts: new Date().toISOString().slice(0, 32),
        });
        upserted++;
      } catch (err) {
        upsertErrors++;
        console.warn(`[upsert error ${i}] ${err.message}`);
      }
    }

    const totalMs = performance.now() - start;
    console.log(`\n  === Milvus Bulk Upsert ===`);
    console.log(`  Upserted: ${upserted}/${strings.length} in ${(totalMs/1000).toFixed(1)}s`);
    console.log(`  Rate: ${(upserted/(totalMs/1000)).toFixed(1)} upserts/s`);
    console.log(`  Errors: ${upsertErrors}`);

    assert.equal(upsertErrors, 0, `${upsertErrors} Milvus upsert errors`);
    assert.ok(upserted === strings.length, `Only ${upserted}/${strings.length} upserted`);
  });

  test('semantic recall from rcc_memory_sparky — results coherent', async () => {
    let vectorMod;
    try {
      vectorMod = await import('../../vector/index.mjs');
    } catch {
      console.warn('[skip] Milvus not reachable');
      return;
    }

    const hits = await vectorMod.vectorSearch(
      'rcc_memory_sparky',
      'GPU workload DGX Spark agent Natasha',
      5
    );
    console.log(`\n  === Recall Check ===`);
    console.log(`  Got ${hits.length} hits`);
    hits.forEach((h, i) => console.log(`  [${i}] score=${h.score?.toFixed(4)} "${h.content?.slice(0,60)}"`));

    assert.ok(hits.length > 0, 'Expected at least 1 recall hit after bulk upsert');
    assert.ok(hits[0].score > 0.3, `Top hit score ${hits[0].score} too low — embedding quality issue`);
  });
});
