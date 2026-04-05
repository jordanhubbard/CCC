#!/usr/bin/env node
/**
 * rcc/scripts/memory-ingest-watcher.mjs — Continuous memory→Milvus+Qdrant ingest watcher
 *
 * Watches ~/.openclaw/workspace/memory/ and MEMORY.md for writes.
 * On change: debounces 10s, then:
 *   1. Embeds via local ollama (nomic-embed-text, 768-dim) → Milvus rcc_memory_sparky
 *   2. Also spawns ingest-memory-qdrant.mjs → fleet Qdrant agent_memories (if keys present)
 *
 * Env vars:
 *   MEMORY_DIR          default: ~/.openclaw/workspace/memory/
 *   MEMORY_FILE         default: ~/.openclaw/workspace/MEMORY.md
 *   AGENT_NAME          default: natasha
 *   MILVUS_ADDRESS      default: 146.190.134.110:19530  (Rocky's Milvus)
 *   MILVUS_COLLECTION   default: rcc_memory_sparky
 *   OLLAMA_BASE_URL     default: http://localhost:11434
 *   OLLAMA_EMBED_MODEL  default: nomic-embed-text
 *   DEBOUNCE_MS         default: 10000
 *   QDRANT_API_KEY      (optional) fleet Qdrant api-key — if set, also ingest to Qdrant
 *   EMBED_API_KEY       (optional) nvidia inference-api key for Qdrant ingest (3072-dim)
 *   QDRANT_URL          default: http://146.190.134.110:6333
 */

import { watch } from 'fs';
import { readFile, readdir } from 'fs/promises';
import { existsSync } from 'fs';
import { spawn } from 'child_process';
import { fileURLToPath } from 'url';
import { dirname } from 'path';
import { join, basename } from 'path';
import { homedir } from 'os';
import { createHash } from 'crypto';

const __dirname = dirname(fileURLToPath(import.meta.url));

const HOME           = homedir();
const AGENT_NAME     = process.env.AGENT_NAME         || 'natasha';
const DEBOUNCE_MS    = parseInt(process.env.DEBOUNCE_MS || '10000', 10);
const OLLAMA_URL     = process.env.OLLAMA_BASE_URL     || 'http://localhost:11434';
const OLLAMA_MODEL   = process.env.OLLAMA_EMBED_MODEL  || 'nomic-embed-text';
const MILVUS_ADDRESS = process.env.MILVUS_ADDRESS      || '146.190.134.110:19530';
const COLLECTION     = process.env.MILVUS_COLLECTION   || 'rcc_memory_sparky';
const MEMORY_DIR     = process.env.MEMORY_DIR          || join(HOME, '.openclaw', 'workspace', 'memory');
const MEMORY_FILE    = process.env.MEMORY_FILE         || join(HOME, '.openclaw', 'workspace', 'MEMORY.md');

function log(msg) {
  console.log(`[memory-watcher ${new Date().toISOString()}] ${msg}`);
}

// Milvus client (lazy)
let _milvus = null;
async function getMilvus() {
  if (!_milvus) {
    const { MilvusClient } = await import('@zilliz/milvus2-sdk-node');
    _milvus = new MilvusClient({ address: MILVUS_ADDRESS });
    log(`Milvus client connected: ${MILVUS_ADDRESS}`);
  }
  return _milvus;
}

// Embed via local ollama
async function embed(text) {
  const resp = await fetch(`${OLLAMA_URL}/api/embeddings`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: OLLAMA_MODEL, prompt: text }),
  });
  if (!resp.ok) throw new Error(`ollama embed HTTP ${resp.status}`);
  const data = await resp.json();
  if (!data.embedding || !data.embedding.length) throw new Error('empty embedding');
  return data.embedding;
}

// Chunk markdown into paragraphs
function chunkMarkdown(content, filePath) {
  const chunks = [];
  const sections = content.split(/\n(?=#{1,3} )/);
  for (const section of sections) {
    const trimmed = section.trim();
    if (!trimmed || trimmed.length < 30) continue;
    const paragraphs = trimmed.split(/\n{2,}/);
    for (const para of paragraphs) {
      const text = para.trim();
      if (!text || text.length < 30) continue;
      const id = createHash('sha256')
        .update(`${AGENT_NAME}:${filePath}:${text.slice(0, 128)}`)
        .digest('hex').slice(0, 32);
      chunks.push({ id, text: text.slice(0, 1500) });
    }
  }
  return chunks;
}

// Ingest one file
async function ingestFile(filePath) {
  if (!existsSync(filePath)) return;
  try {
    const content = await readFile(filePath, 'utf8');
    if (!content.trim()) return;

    const chunks = chunkMarkdown(content, filePath);
    if (!chunks.length) { log(`${basename(filePath)}: no chunks`); return; }

    log(`${basename(filePath)}: ${chunks.length} chunks → ${COLLECTION}`);
    const milvus = await getMilvus();
    let ok = 0, fail = 0;

    for (const chunk of chunks) {
      try {
        const vector = await embed(chunk.text);
        await milvus.upsert({
          collection_name: COLLECTION,
          data: [{
            id:     chunk.id,
            vector,
            text:   chunk.text,
            agent:  AGENT_NAME,
            source: filePath,
            scope:  filePath.includes('MEMORY.md') ? 'fleet' : 'private',
            ts:     Date.now(),
          }],
        });
        ok++;
      } catch (e) {
        log(`  WARN chunk ${chunk.id.slice(0,8)}: ${e.message}`);
        fail++;
      }
    }
    log(`✓ ${basename(filePath)}: ${ok} upserted, ${fail} failed`);
  } catch (e) {
    log(`✗ ${basename(filePath)}: ${e.message}`);
  }
}

// Debounce map
const pending = new Map();
function scheduleIngest(filePath) {
  if (pending.has(filePath)) clearTimeout(pending.get(filePath));
  pending.set(filePath, setTimeout(async () => {
    pending.delete(filePath);
    await ingestFile(filePath);
  }, DEBOUNCE_MS));
}

// Backfill all existing memory files
async function backfill() {
  log('Backfill starting...');
  try {
    const files = await readdir(MEMORY_DIR);
    for (const f of files.filter(f => f.endsWith('.md'))) {
      await ingestFile(join(MEMORY_DIR, f));
    }
  } catch (e) { log(`WARN backfill dir: ${e.message}`); }
  if (existsSync(MEMORY_FILE)) await ingestFile(MEMORY_FILE);
  log('Backfill done.');
}

async function main() {
  log(`Starting — agent=${AGENT_NAME} milvus=${MILVUS_ADDRESS} collection=${COLLECTION} debounce=${DEBOUNCE_MS}ms`);

  // Warm up Milvus connection
  try { await getMilvus(); } catch (e) { log(`WARN milvus warmup: ${e.message}`); }

  await backfill();

  // Watch memory dir
  if (existsSync(MEMORY_DIR)) {
    log(`Watching dir: ${MEMORY_DIR}`);
    watch(MEMORY_DIR, { recursive: false }, (event, filename) => {
      if (filename && filename.endsWith('.md')) {
        log(`change: ${filename} (${event})`);
        scheduleIngest(join(MEMORY_DIR, filename));
      }
    });
  } else {
    log(`WARN: memory dir not found: ${MEMORY_DIR}`);
  }

  // Watch MEMORY.md
  if (existsSync(MEMORY_FILE)) {
    log(`Watching file: ${MEMORY_FILE}`);
    watch(MEMORY_FILE, (event) => {
      log(`change: MEMORY.md (${event})`);
      scheduleIngest(MEMORY_FILE);
    });
  } else {
    log(`WARN: MEMORY.md not found: ${MEMORY_FILE}`);
  }

  log('Watcher active.');
}

main().catch(e => { console.error(e); process.exit(1); });
