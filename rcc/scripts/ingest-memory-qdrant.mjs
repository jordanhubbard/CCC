#!/usr/bin/env node
/**
 * rcc/scripts/ingest-memory-qdrant.mjs — Ingest agent memories into fleet Qdrant
 *
 * Ingests MEMORY.md + memory/*.md into the fleet Qdrant instance.
 * Uses azure/openai/text-embedding-3-large (3072-dim) via inference-api.nvidia.com.
 *
 * Env:
 *   QDRANT_URL       (default: http://146.190.134.110:6333)
 *   QDRANT_API_KEY   (required)
 *   EMBED_API_KEY    (required: inference-api.nvidia.com key)
 *   AGENT_NAME       (default: natasha)
 *
 * Run: QDRANT_API_KEY=... EMBED_API_KEY=... node rcc/scripts/ingest-memory-qdrant.mjs
 */

import { readFile, readdir } from 'fs/promises';
import { existsSync } from 'fs';
import { join, dirname } from 'path';
import { homedir } from 'os';
import { createHash } from 'crypto';

const HOME = homedir();

// ── Config ───────────────────────────────────────────────────────────────────
const QDRANT_URL     = process.env.QDRANT_URL     || 'http://146.190.134.110:6333';
const QDRANT_API_KEY = process.env.QDRANT_API_KEY || '';
const EMBED_URL      = 'https://inference-api.nvidia.com/v1/embeddings';
const EMBED_API_KEY  = process.env.EMBED_API_KEY  || '';
const EMBED_MODEL    = 'azure/openai/text-embedding-3-large';
const EMBED_DIM      = 3072;
const AGENT_NAME     = process.env.AGENT_NAME     || 'natasha';
const COLLECTION     = 'agent_memories';
const BATCH_SIZE     = 4;

if (!QDRANT_API_KEY) { console.error('QDRANT_API_KEY required'); process.exit(1); }
if (!EMBED_API_KEY)  { console.error('EMBED_API_KEY required'); process.exit(1); }

// ── Qdrant helpers ───────────────────────────────────────────────────────────
const qdrantHeaders = () => ({ 'Content-Type': 'application/json', 'api-key': QDRANT_API_KEY });

async function getCollectionDim() {
  const r = await fetch(`${QDRANT_URL}/collections/${COLLECTION}`, { headers: qdrantHeaders() });
  if (!r.ok) return null;
  const d = await r.json();
  return d.result?.config?.params?.vectors?.size || null;
}

async function upsertPoints(points) {
  const r = await fetch(`${QDRANT_URL}/collections/${COLLECTION}/points?wait=true`, {
    method: 'PUT', headers: qdrantHeaders(),
    body: JSON.stringify({ points })
  });
  if (!r.ok) throw new Error(`upsert ${r.status}: ${await r.text()}`);
}

// ── Embed ────────────────────────────────────────────────────────────────────
async function embed(texts, retries = 3) {
  for (let attempt = 0; attempt < retries; attempt++) {
    try {
      const r = await fetch(EMBED_URL, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${EMBED_API_KEY}` },
        body: JSON.stringify({ model: EMBED_MODEL, input: texts, encoding_format: 'float' }),
        signal: AbortSignal.timeout(90000)
      });
      if (r.status === 429) {
        const wait = 5000 * (attempt + 1);
        console.log(`\n  [rate-limit] waiting ${wait}ms...`);
        await new Promise(r => setTimeout(r, wait));
        continue;
      }
      if (!r.ok) throw new Error(`embed ${r.status}: ${await r.text()}`);
      const d = await r.json();
      return d.data.map(x => x.embedding);
    } catch (err) {
      if (attempt < retries - 1) {
        console.log(`\n  [embed retry ${attempt+1}] ${err.message}`);
        await new Promise(r => setTimeout(r, 3000 * (attempt + 1)));
      } else throw err;
    }
  }
}

// ── Chunker ──────────────────────────────────────────────────────────────────
function chunkMarkdown(content, filePath) {
  const chunks = [];
  const sections = content.split(/\n(?=#{1,3} )/);
  for (const section of sections) {
    const trimmed = section.trim();
    if (!trimmed || trimmed.length < 20) continue;
    const paragraphs = trimmed.split(/\n{2,}/);
    for (const para of paragraphs) {
      const text = para.trim();
      if (!text || text.length < 20) continue;
      const hash = createHash('md5').update(`${filePath}:${text.slice(0, 100)}`).digest('hex');
      // Use first 8 hex chars as a safe positive integer ID
      const id = parseInt(hash.slice(0, 8), 16);
      chunks.push({ id, text: text.slice(0, 1400), payload: { source: filePath, agent: AGENT_NAME, text: text.slice(0, 1400) } });
    }
  }
  return chunks;
}

// ── Ingest one file ──────────────────────────────────────────────────────────
async function ingestFile(filePath) {
  const content = await readFile(filePath, 'utf8');
  const chunks = chunkMarkdown(content, filePath);
  if (!chunks.length) { console.log(`  ${filePath} → 0 chunks (skipped)`); return 0; }

  let total = 0;
  for (let i = 0; i < chunks.length; i += BATCH_SIZE) {
    const batch = chunks.slice(i, i + BATCH_SIZE);
    const vecs  = await embed(batch.map(c => c.text));
    const points = batch.map((c, j) => ({ id: c.id, vector: vecs[j], payload: c.payload }));
    await upsertPoints(points);
    total += batch.length;
    process.stdout.write(`\r  ${filePath} → ${total}/${chunks.length} chunks`);
  }
  console.log(`\r  ${filePath} → ${total} chunks ✓   `);
  return total;
}

// ── Resolve paths ─────────────────────────────────────────────────────────────
function resolveMemoryDir() {
  if (process.env.MEMORY_DIR && existsSync(process.env.MEMORY_DIR)) return process.env.MEMORY_DIR;
  const oc = join(HOME, '.openclaw', 'workspace', 'memory');
  if (existsSync(oc)) return oc;
  const rcc = join(HOME, '.rcc', 'workspace', 'memory');
  if (existsSync(rcc)) return rcc;
  return null;
}

function resolveMemoryMd(memDir) {
  if (process.env.MEMORY_FILE && existsSync(process.env.MEMORY_FILE)) return process.env.MEMORY_FILE;
  if (!memDir) return null;
  const sib = join(dirname(memDir), 'MEMORY.md');
  return existsSync(sib) ? sib : null;
}

// ── Main ─────────────────────────────────────────────────────────────────────
console.log(`[ingest-memory-qdrant] agent=${AGENT_NAME}, target=${QDRANT_URL}/${COLLECTION}`);
console.log(`[ingest-memory-qdrant] embed model: ${EMBED_MODEL} (${EMBED_DIM}-dim)`);

const existingDim = await getCollectionDim();
if (existingDim && existingDim !== EMBED_DIM) {
  console.error(`Collection dim mismatch: expected ${EMBED_DIM}, got ${existingDim}`);
  process.exit(1);
}
console.log(`[ingest-memory-qdrant] Collection '${COLLECTION}' dim=${existingDim || EMBED_DIM} ✓`);

const memDir = resolveMemoryDir();
const memMd  = resolveMemoryMd(memDir);

const files = [];
if (memMd) files.push(memMd);
if (memDir) {
  const entries = await readdir(memDir);
  files.push(...entries.filter(e => e.endsWith('.md')).map(e => join(memDir, e)));
}

if (!files.length) { console.error('No memory files found.'); process.exit(1); }
console.log(`[ingest-memory-qdrant] ${files.length} file(s) to ingest\n`);

let grand = 0;
for (const f of files) grand += await ingestFile(f);
console.log(`\n[ingest-memory-qdrant] Done. ${grand} total chunks upserted into fleet Qdrant.`);
