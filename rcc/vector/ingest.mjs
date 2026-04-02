/**
 * rcc/vector/ingest.mjs — Ingest helpers for Milvus RAG pipeline
 *
 * Provides fire-and-forget ingest functions for all write paths:
 *   - Memory files (.md)
 *   - Queue items
 *   - Lessons
 *   - ClawChat messages
 *
 * All failures are logged but never thrown — ingest is best-effort.
 *
 * Backend selection:
 *   EMBED_BACKEND=local  → uses ollama nomic-embed-text (768-dim) on sparky
 *                          memory files go to rcc_memory_sparky collection
 *   EMBED_BACKEND=remote → uses NVIDIA NIM text-embedding-3-large (3072-dim) [default]
 *                          memory files go to rcc_memory collection
 */

import { readFile } from 'fs/promises';
import { createHash } from 'crypto';
import { vectorUpsert, vectorUpsertBatch, ensureCollections } from './index.mjs';

// When EMBED_BACKEND=local, route memory ingest to the sparky-local collection
const EMBED_BACKEND = process.env.EMBED_BACKEND || 'remote';
const MEMORY_COLLECTION = EMBED_BACKEND === 'local' ? 'rcc_memory_sparky' : 'rcc_memory';

let collectionsReady = false;

async function ensureReady() {
  if (!collectionsReady) {
    await ensureCollections();
    collectionsReady = true;
  }
}

/**
 * Chunk a markdown file by headings + paragraphs.
 * Returns [{id, text, metadata}]
 */
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
      const id = createHash('md5').update(`${filePath}:${text.slice(0, 100)}`).digest('hex');
      chunks.push({ id, text: text.slice(0, 1000), metadata: { source: filePath, type: 'memory' } });
    }
  }
  return chunks;
}

/**
 * Ingest a markdown memory file into the active memory collection.
 * Uses rcc_memory_sparky (768-dim local) when EMBED_BACKEND=local,
 * or rcc_memory (3072-dim remote) otherwise.
 */
export async function ingestMemoryFile(filePath) {
  try {
    await ensureReady();
    const content = await readFile(filePath, 'utf8');
    const chunks = chunkMarkdown(content, filePath);
    let count = 0;
    for (const chunk of chunks) {
      await vectorUpsert(MEMORY_COLLECTION, chunk.id, chunk.text, chunk.metadata).catch(() => {});
      count++;
    }
    console.log(`[ingest] ${filePath} → ${count}/${chunks.length} chunks (${MEMORY_COLLECTION}, backend=${EMBED_BACKEND})`);
  } catch (err) {
    console.warn(`[ingest] Failed to ingest ${filePath}:`, err.message);
  }
}

/**
 * Ingest a queue item into rcc_queue collection.
 */
export async function ingestQueueItem(item) {
  try {
    await ensureReady();
    const text = [item.title, item.description, item.notes].filter(Boolean).join('\n').slice(0, 1000);
    if (!text || text.length < 10) return;
    const id = createHash('md5').update(`queue:${item.id}`).digest('hex');
    await vectorUpsert('rcc_queue', id, text, {
      title:       (item.title || '').slice(0, 256),
      description: (item.description || '').slice(0, 2048),
      status:      (item.status || 'pending').slice(0, 32),
      priority:    (item.priority || 'normal').slice(0, 16),
      tags:        (Array.isArray(item.tags) ? item.tags.join(',') : (item.tags || '')).slice(0, 256),
      ts:          new Date(item.created_at || Date.now()).toISOString().slice(0, 32),
    });
  } catch (err) {
    console.warn(`[ingest] Failed to ingest queue item ${item.id}:`, err.message);
  }
}

/**
 * Ingest a lesson into rcc_lessons collection.
 */
export async function ingestLesson(lesson) {
  try {
    await ensureReady();
    const text = [lesson.symptom, lesson.fix, lesson.context].filter(Boolean).join('\n').slice(0, 1000);
    if (!text || text.length < 10) return;
    const id = createHash('md5').update(`lesson:${lesson.id || text.slice(0, 50)}`).digest('hex');
    await vectorUpsert('rcc_lessons', id, text, {
      source: `lesson:${lesson.id || 'unknown'}`, type: 'lesson'
    });
  } catch (err) {
    console.warn(`[ingest] Failed to ingest lesson:`, err.message);
  }
}

/**
 * Ingest a ClawChat message into the active memory collection.
 * Routes to rcc_memory_sparky (768-dim, GPU) when EMBED_BACKEND=local,
 * or rcc_memory (3072-dim, Azure) otherwise.
 * On sparky, set EMBED_BACKEND=local for zero-cost GPU-accelerated ingest.
 */
export async function ingestMessage(msg) {
  try {
    await ensureReady();
    const text = msg.text || '';
    if (!text || text.length < 5) return;
    const id = createHash('md5').update(`squirrelchat:${msg.id}:${msg.ts}`).digest('hex');
    await vectorUpsert(MEMORY_COLLECTION, id, text.slice(0, 1000), {
      agent:      (msg.from_agent || 'unknown').slice(0, 32),
      content:    text.slice(0, 4096),
      source:     `squirrelchat:${msg.id || 'unknown'}`,
      ts:         new Date(msg.ts || Date.now()).toISOString().slice(0, 32),
    });
  } catch (err) {
    console.warn(`[ingest] Failed to ingest message ${msg.id}:`, err.message);
  }
}

/**
 * Batch ingest multiple ClawChat messages in a single embedding call.
 * Uses vectorUpsertBatch → embedBatchLocal (adaptive batching on GB10 GPU).
 * ~100x faster than calling ingestMessage in a loop for large backlogs.
 *
 * @param {object[]} msgs - Array of message objects (same shape as ingestMessage)
 */
export async function ingestMessages(msgs) {
  if (!msgs || !msgs.length) return;
  try {
    await ensureReady();
    const items = msgs
      .filter(msg => msg.text && msg.text.length >= 5)
      .map(msg => ({
        id:   createHash('md5').update(`squirrelchat:${msg.id}:${msg.ts}`).digest('hex'),
        text: (msg.text || '').slice(0, 1000),
        meta: {
          agent:   (msg.from_agent || 'unknown').slice(0, 32),
          content: (msg.text || '').slice(0, 4096),
          source:  `squirrelchat:${msg.id || 'unknown'}`,
          ts:      new Date(msg.ts || Date.now()).toISOString().slice(0, 32),
        },
      }));
    if (!items.length) return;
    await vectorUpsertBatch(MEMORY_COLLECTION, items);
    console.log(`[ingest] batch ingested ${items.length}/${msgs.length} messages (${MEMORY_COLLECTION})`);
  } catch (err) {
    console.warn(`[ingest] ingestMessages batch failed, falling back to serial:`, err.message);
    // Graceful fallback to one-at-a-time
    for (const msg of msgs) {
      await ingestMessage(msg).catch(() => {});
    }
  }
}

/**
 * Semantic search over ingested ClawChat messages.
 * Uses the same collection as ingestMessage (local or remote depending on EMBED_BACKEND).
 * @param {string} query   - Natural language search query
 * @param {number} limit   - Max results (default 5)
 * @param {string} agent   - Filter by agent name (optional)
 * @returns {Promise<object[]>} - Array of matching message snippets with scores
 */
export async function recallClawChat(query, limit = 5, agent = '') {
  try {
    await ensureReady();
    const { vectorSearch } = await import('./index.mjs');
    const filter = agent ? `agent == "${agent}"` : '';
    const hits = await vectorSearch(MEMORY_COLLECTION, query, limit, filter);
    return hits.map(h => ({
      content: h.content,
      agent: h.agent,
      source: h.source,
      ts: h.ts,
      score: h.score,
    }));
  } catch (err) {
    console.warn(`[ingest] recallClawChat failed:`, err.message);
    return [];
  }
}
